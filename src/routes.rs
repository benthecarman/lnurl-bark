use crate::models::custom_address_purchase::{CustomAddressPurchase, NewCustomAddressPurchase};
use crate::models::invoice::{Invoice, InvoiceState, NewInvoice};
use crate::models::user::{NewUser, User};
use crate::models::zap::Zap;
use crate::State;
use anyhow::anyhow;
use axum::extract::{Path, Query};
use axum::http::{StatusCode, Uri};
use axum::{Extension, Json};
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::schnorr::Signature;
use chrono::{DateTime, NaiveDateTime, Utc};
use diesel::Connection;
use lightning_invoice::Bolt11Invoice;
use lightning_invoice::Bolt11InvoiceDescriptionRef;
use lnurl::pay::PayResponse;
use lnurl::Tag;
use log::error;
use nostr::{Event, JsonUtil};
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::fmt::Display;
use std::str::FromStr;
use std::time::SystemTime;

const MAX_NAME_LEN: usize = 64;
const MAX_COMMENT_LEN: usize = 100;
const MAX_NOSTR_PARAM_LEN: usize = 16 * 1024;
const MAX_SIGNATURE_LEN: usize = 128;

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LnurlCallbackParams {
    pub amount: Option<u64>, // User specified amount in MilliSatoshi
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub comment: Option<String>, // Optional parameter to pass the LN WALLET user's comment to LN SERVICE
    #[serde(default, deserialize_with = "empty_string_as_none")]
    pub nostr: Option<String>, // Optional zap request
}

/// Creates a Lightning invoice and optionally stores zap request information.
///
/// This is the core implementation for generating invoices for LNURL-pay requests.
///
/// # Parameters
/// * `state` - Application state containing LND client and configuration
/// * `hash` - A description hash or identifier for the invoice
/// * `amount_msats` - The invoice amount in millisatoshis
/// * `zap_request` - Optional Nostr zap request event
///
/// # Returns
/// A BOLT11 invoice if successful, or an error
pub(crate) async fn get_invoice_impl(
    state: &State,
    name: &str,
    params: LnurlCallbackParams,
) -> anyhow::Result<Bolt11Invoice> {
    validate_name(name)?;
    validate_callback_params(&params)?;

    if params.amount.is_none() {
        return Err(anyhow!("Missing amount parameter"));
    }
    let amount_msats = params.amount.unwrap();
    validate_amount_msats(amount_msats, state.min_sendable, state.max_sendable)?;

    let mut conn = state.db_pool.get()?;

    let user = User::get_active_by_name(&mut conn, name)?.ok_or(anyhow!("User not found"))?;
    let ark_address = user.ark_address();

    if user.disabled_zaps {
        return Err(anyhow!("Zaps are disabled for this user"));
    }

    let mut zap_request = None;
    let _invoice_description = match params.nostr.as_ref() {
        None => calc_metadata(name, &state.domain),
        Some(str) => {
            let event = Event::from_json(str).map_err(|_| anyhow!("Invalid zap request"))?;
            if event.kind != nostr::Kind::ZapRequest {
                return Err(anyhow!("Invalid zap request"));
            }
            zap_request = Some(event);
            str.clone()
        }
    };

    let invoice = state
        .barkd
        .invoice_for_address(
            amount_msats / 1_000,
            ark_address.to_string(),
            Some(_invoice_description),
        )
        .await?;

    if !invoice
        .amount_milli_satoshis()
        .is_some_and(|a| a == amount_msats)
    {
        return Err(anyhow!("Invoice amount mismatch"));
    }

    let payment_hash = invoice.payment_hash().to_string();
    let expires_at = invoice_expires_at(&invoice);

    conn.transaction::<_, anyhow::Error, _>(|conn| {
        let invoice = NewInvoice {
            user_id: user.id,
            bolt11: invoice.to_string(),
            amount_msats: amount_msats as i64,
            payment_hash: Some(payment_hash),
            preimage: String::new(),
            lnurlp_comment: params.comment,
            state: InvoiceState::Pending as i32,
            expires_at,
        };
        let inserted_invoice = invoice.insert(conn)?;

        if let Some(zap_request) = zap_request {
            let zap = Zap {
                id: inserted_invoice.id,
                request: zap_request.as_json(),
                event_id: None,
            };
            zap.insert(conn)?;
        }

        Ok(())
    })?;

    Ok(invoice)
}

fn invoice_expires_at(invoice: &Bolt11Invoice) -> Option<NaiveDateTime> {
    let expires_at = invoice.expires_at()?;
    let expires_at = SystemTime::UNIX_EPOCH.checked_add(expires_at)?;
    Some(DateTime::<Utc>::from(expires_at).naive_utc())
}

/// HTTP endpoint for generating Lightning invoices from a LNURL-pay request.
///
/// This route handles the callback phase of the LNURL-pay protocol.
///
/// # Parameters
/// * `hash` - Path parameter containing the description hash
/// * `params` - Query parameters including the amount and optional zap request
/// * `state` - Application state
///
/// # Returns
/// A JSON response with the invoice and verification URL, or an error response
pub async fn get_invoice(
    Path(name): Path<String>,
    Query(params): Query<LnurlCallbackParams>,
    Extension(state): Extension<State>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let amount_msats = params.amount;

    match get_invoice_impl(&state, &name, params).await {
        Ok(invoice) => {
            let desc_hash = invoice_description_hash(&invoice);
            let payment_hash = invoice.payment_hash().to_string();
            let verify_url = format!("https://{}/verify/{desc_hash}/{payment_hash}", state.domain);
            Ok(Json(json!({
                "status": "OK",
                "pr": invoice,
                "verify": verify_url,
                "routes": [],
            })))
        }
        Err(e) => {
            error!("Error generating invoice for name={name} amount_msats={amount_msats:?}: {e:#}");
            Err(handle_anyhow_error(e))
        }
    }
}

pub fn calc_metadata(name: &str, domain: &str) -> String {
    format!("[[\"text/identifier\",\"{name}@{domain}\"],[\"text/plain\",\"Sats for {name}\"]]",)
}

fn invoice_description_hash(invoice: &Bolt11Invoice) -> String {
    match invoice.description() {
        Bolt11InvoiceDescriptionRef::Direct(description) => {
            sha256::Hash::hash(description.to_string().as_bytes()).to_string()
        }
        Bolt11InvoiceDescriptionRef::Hash(hash) => hex::encode(hash.0.to_byte_array()),
    }
}

fn validate_amount_msats(
    amount_msats: u64,
    min_sendable: u64,
    max_sendable: u64,
) -> anyhow::Result<()> {
    if amount_msats < min_sendable || amount_msats > max_sendable {
        return Err(anyhow!("Amount out of bounds"));
    }
    if amount_msats % 1_000 != 0 {
        return Err(anyhow!("Bark invoices must be denominated in whole sats"));
    }

    Ok(())
}

fn validate_callback_params(params: &LnurlCallbackParams) -> anyhow::Result<()> {
    if params
        .comment
        .as_ref()
        .is_some_and(|comment| comment.chars().count() > MAX_COMMENT_LEN)
    {
        return Err(anyhow!("Comment is too long"));
    }

    if params
        .nostr
        .as_ref()
        .is_some_and(|nostr| nostr.len() > MAX_NOSTR_PARAM_LEN)
    {
        return Err(anyhow!("Nostr parameter is too large"));
    }

    Ok(())
}

fn validate_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        return Err(anyhow!("Name parameter is required"));
    }

    if name.len() > MAX_NAME_LEN {
        return Err(anyhow!("Name parameter is too long"));
    }

    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        return Err(anyhow!("Name parameter contains invalid characters"));
    }

    Ok(())
}

/// HTTP endpoint that provides the LNURL-pay metadata and parameters.
///
/// This is the entry point for the LNURL-pay protocol, served at the .well-known/lnurlp/{name} path.
///
/// # Parameters
/// * `name` - Path parameter containing the username portion of the Lightning address
/// * `state` - Application state with domain and configuration
///
/// # Returns
/// A LNURL PayResponse with callback URL and other parameters, or an error response
pub async fn get_lnurl_pay(
    Path(name): Path<String>,
    Extension(state): Extension<State>,
) -> Result<Json<PayResponse>, (StatusCode, Json<Value>)> {
    if let Err(e) = validate_name(&name) {
        return Err(handle_anyhow_error(e));
    }

    let mut conn = state.db_pool.get().map_err(|e| {
        error!("DB connection error: {e}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "status": "ERROR",
                "reason": "Server error",
            })),
        )
    })?;

    if User::get_active_by_name(&mut conn, &name)
        .map_err(|e| {
            error!("Error looking up user for LNURL metadata: {e:?}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({
                    "status": "ERROR",
                    "reason": "Server error",
                })),
            )
        })?
        .is_none()
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "ERROR",
                "reason": "User not found",
            })),
        ));
    }

    let metadata = calc_metadata(&name, &state.domain);

    let callback = format!("https://{}/get-invoice/{name}", state.domain);

    let resp = PayResponse {
        callback,
        min_sendable: state.min_sendable,
        max_sendable: state.max_sendable,
        tag: Tag::PayRequest,
        metadata,
        comment_allowed: Some(100),
        allows_nostr: Some(true),
        nostr_pubkey: Some(
            state
                .keys
                .public_key()
                .xonly()
                .expect("cant get xonly pubkey"),
        ),
    };

    Ok(Json(resp))
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RegisterQuoteParams {
    pub name: String,
    #[serde(alias = "ark_address")]
    pub ark_address: String,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RegisterRequest {
    pub name: String,
    #[serde(alias = "ark_address")]
    pub ark_address: String,
    pub signature: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterQuoteResponse {
    pub name: String,
    pub lightning_address: String,
    pub ark_address: String,
    pub amount_msats: u64,
    pub message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterResponse {
    pub status: String,
    pub name: String,
    pub lightning_address: String,
    pub ark_address: String,
    pub amount_msats: Option<u64>,
    pub pr: Option<String>,
    pub payment_hash: Option<String>,
    pub verify: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RegisterStatusResponse {
    pub status: String,
    pub name: String,
    pub lightning_address: String,
    pub ark_address: String,
    pub amount_msats: u64,
    pub paid: bool,
    pub active: bool,
    pub pr: String,
}

pub async fn register_quote(
    state: &State,
    params: RegisterQuoteParams,
) -> anyhow::Result<RegisterQuoteResponse> {
    let ark_address = validate_registration_inputs(&params.name, &params.ark_address, None)?;
    let ark_address = ark_address.to_string();

    let mut conn = state.db_pool.get()?;
    if User::get_by_name(&mut conn, &params.name)?.is_some() {
        return Err(anyhow!("NameTaken"));
    }

    let amount_msats = custom_address_fee_msats(state)?;

    Ok(RegisterQuoteResponse {
        name: params.name.clone(),
        lightning_address: lightning_address(&params.name, &state.domain),
        ark_address: ark_address.clone(),
        amount_msats,
        message: custom_address_auth_message(&state.domain, &params.name, &ark_address),
    })
}

pub async fn register(state: &State, req: RegisterRequest) -> anyhow::Result<RegisterResponse> {
    let ark_address =
        validate_registration_inputs(&req.name, &req.ark_address, Some(req.signature.as_str()))?;
    let ark_address = ark_address.to_string();
    let auth_message = custom_address_auth_message(&state.domain, &req.name, &ark_address);
    verify_registration_signature(&ark_address, &auth_message, &req.signature)?;

    let mut conn = state.db_pool.get()?;
    let amount_msats = custom_address_fee_msats(state)?;
    let amount_msats_i64 =
        i64::try_from(amount_msats).map_err(|_| anyhow!("Registration fee is too large"))?;

    if let Some(existing_user) = User::get_by_name(&mut conn, &req.name)? {
        if existing_user.activated_at.is_some() {
            if existing_user.ark_address == ark_address {
                return Ok(active_registration_response(
                    state,
                    &existing_user.name,
                    &existing_user.ark_address,
                ));
            }
            return Err(anyhow!("NameTaken"));
        }

        if existing_user.ark_address != ark_address {
            return Err(anyhow!("NameTaken"));
        }

        let purchase = CustomAddressPurchase::get_by_user_id(&mut conn, existing_user.id)?
            .ok_or_else(|| anyhow!("PendingRegistrationIncomplete"))?;
        let invoice = Invoice::get_by_id(&mut conn, purchase.invoice_id)?
            .ok_or_else(|| anyhow!("PendingRegistrationInvoiceMissing"))?;

        if invoice.state == InvoiceState::Settled as i32 {
            let purchase = CustomAddressPurchase::activate_for_invoice(&mut conn, invoice.id)?
                .ok_or_else(|| anyhow!("PendingRegistrationIncomplete"))?;
            return registration_response_from_invoice(state, &invoice, &purchase);
        }

        if invoice.state == InvoiceState::Pending as i32 && !invoice_has_expired(&invoice) {
            return registration_response_from_invoice(state, &invoice, &purchase);
        }

        drop(conn);
        return create_registration_invoice(
            state,
            existing_user.id,
            &req.name,
            &ark_address,
            &auth_message,
            &req.signature,
            amount_msats,
            amount_msats_i64,
            Some(invoice.id),
        )
        .await;
    }

    let new_user = NewUser {
        ark_address: ark_address.clone(),
        name: req.name.clone(),
        activated_at: None,
    };
    let user = new_user.insert(&mut conn)?;
    drop(conn);

    create_registration_invoice(
        state,
        user.id,
        &req.name,
        &ark_address,
        &auth_message,
        &req.signature,
        amount_msats,
        amount_msats_i64,
        None,
    )
    .await
}

async fn create_registration_invoice(
    state: &State,
    user_id: i32,
    name: &str,
    ark_address: &str,
    auth_message: &str,
    signature: &str,
    amount_msats: u64,
    amount_msats_i64: i64,
    replace_invoice_id: Option<i32>,
) -> anyhow::Result<RegisterResponse> {
    let amount_sat = amount_msats
        .checked_div(1_000)
        .ok_or_else(|| anyhow!("InvalidRegistrationFee"))?;
    let invoice = match state
        .barkd
        .wallet_invoice(
            amount_sat,
            Some(custom_address_invoice_description(
                &state.domain,
                name,
                ark_address,
            )),
        )
        .await
    {
        Ok(invoice) => invoice,
        Err(e) => {
            if replace_invoice_id.is_none() {
                cleanup_pending_user(state, user_id);
            }
            return Err(e);
        }
    };

    if !invoice
        .amount_milli_satoshis()
        .is_some_and(|amount| amount == amount_msats)
    {
        if replace_invoice_id.is_none() {
            cleanup_pending_user(state, user_id);
        }
        return Err(anyhow!("Invoice amount mismatch"));
    }

    let payment_hash = invoice.payment_hash().to_string();
    let expires_at = invoice_expires_at(&invoice);
    let mut conn = state.db_pool.get()?;
    let inserted = conn.transaction::<_, anyhow::Error, _>(|conn| {
        let inserted_invoice = NewInvoice {
            user_id,
            bolt11: invoice.to_string(),
            amount_msats: amount_msats_i64,
            payment_hash: Some(payment_hash),
            preimage: String::new(),
            lnurlp_comment: None,
            state: InvoiceState::Pending as i32,
            expires_at,
        }
        .insert(conn)?;

        let purchase = if let Some(replace_invoice_id) = replace_invoice_id {
            if let Some(old_invoice) = Invoice::get_by_id(conn, replace_invoice_id)? {
                old_invoice.mark_cancelled(conn)?;
            }
            CustomAddressPurchase::replace_invoice_for_user(
                conn,
                user_id,
                inserted_invoice.id,
                auth_message.to_string(),
                signature.to_string(),
                amount_msats_i64,
            )?
        } else {
            NewCustomAddressPurchase {
                invoice_id: inserted_invoice.id,
                user_id,
                name: name.to_string(),
                ark_address: ark_address.to_string(),
                auth_message: auth_message.to_string(),
                signature: signature.to_string(),
                fee_msats: amount_msats_i64,
            }
            .insert(conn)?
        };

        Ok((inserted_invoice, purchase))
    });

    match inserted {
        Ok((inserted_invoice, purchase)) => {
            registration_response_from_invoice(state, &inserted_invoice, &purchase)
        }
        Err(e) => {
            if replace_invoice_id.is_none() {
                cleanup_pending_user(state, user_id);
            }
            Err(e)
        }
    }
}

pub async fn register_quote_route(
    Query(params): Query<RegisterQuoteParams>,
    Extension(state): Extension<State>,
) -> Result<Json<RegisterQuoteResponse>, (StatusCode, Json<Value>)> {
    register_quote(&state, params)
        .await
        .map(Json)
        .map_err(handle_anyhow_error)
}

pub async fn register_route(
    Extension(state): Extension<State>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, (StatusCode, Json<Value>)> {
    register(&state, req)
        .await
        .map(Json)
        .map_err(handle_anyhow_error)
}

pub async fn register_status_route(
    Path(payment_hash): Path<String>,
    Extension(state): Extension<State>,
) -> Result<Json<RegisterStatusResponse>, (StatusCode, Json<Value>)> {
    validate_hex_hash(&payment_hash, "Invalid payment hash")?;

    let mut invoice = find_invoice_by_payment_hash(&state, &payment_hash)?;
    let purchase = find_purchase_for_invoice(&state, invoice.id)?;

    if invoice.state == InvoiceState::Pending as i32 {
        refresh_invoice_receive_status(&state, &invoice, &payment_hash).await?;
        invoice = find_invoice_by_payment_hash(&state, &payment_hash)?;
    }

    let purchase = if invoice.state == InvoiceState::Settled as i32 {
        activate_custom_address_purchase(&state, invoice.id)?
            .ok_or_else(|| server_error_response())?
    } else {
        purchase
    };

    Ok(Json(registration_status_response(
        &state, &invoice, &purchase,
    )))
}

fn validate_registration_inputs(
    name: &str,
    ark_address: &str,
    signature: Option<&str>,
) -> anyhow::Result<ark::Address> {
    validate_name(name)?;

    if let Some(signature) = signature {
        if signature.is_empty() || signature.len() > MAX_SIGNATURE_LEN {
            return Err(anyhow!("InvalidSignature"));
        }
    }

    ark_address
        .parse::<ark::Address>()
        .map_err(|_| anyhow!("InvalidArkAddress"))
}

fn verify_registration_signature(
    ark_address: &str,
    auth_message: &str,
    signature: &str,
) -> anyhow::Result<()> {
    let ark_address = ark_address
        .parse::<ark::Address>()
        .map_err(|_| anyhow!("InvalidArkAddress"))?;
    let signature = Signature::from_str(signature).map_err(|_| anyhow!("InvalidSignature"))?;

    ark_address
        .verify_message(auth_message.as_bytes(), &signature)
        .map_err(|_| anyhow!("InvalidSignature"))
}

fn custom_address_fee_msats(state: &State) -> anyhow::Result<u64> {
    state
        .custom_address_fee_sats
        .checked_mul(1_000)
        .ok_or_else(|| anyhow!("Registration fee is too large"))
}

fn custom_address_auth_message(domain: &str, name: &str, ark_address: &str) -> String {
    format!(
        "lnurl-bark custom address registration\nDomain: {domain}\nName: {name}\nArk address: {ark_address}"
    )
}

fn custom_address_invoice_description(domain: &str, name: &str, ark_address: &str) -> String {
    format!("Custom Lightning address registration for {name}@{domain} ({ark_address})")
}

fn lightning_address(name: &str, domain: &str) -> String {
    format!("{name}@{domain}")
}

fn cleanup_pending_user(state: &State, user_id: i32) {
    match state.db_pool.get() {
        Ok(mut conn) => {
            if let Err(e) = User::delete_by_id(&mut conn, user_id) {
                error!("Error cleaning up pending registration user {user_id}: {e:?}");
            }
        }
        Err(e) => error!("DB connection error while cleaning up pending user {user_id}: {e}"),
    }
}

fn active_registration_response(state: &State, name: &str, ark_address: &str) -> RegisterResponse {
    RegisterResponse {
        status: "ACTIVE".to_string(),
        name: name.to_string(),
        lightning_address: lightning_address(name, &state.domain),
        ark_address: ark_address.to_string(),
        amount_msats: None,
        pr: None,
        payment_hash: None,
        verify: None,
    }
}

fn registration_response_from_invoice(
    state: &State,
    invoice: &Invoice,
    purchase: &CustomAddressPurchase,
) -> anyhow::Result<RegisterResponse> {
    let payment_hash = invoice_payment_hash_string(invoice);
    Ok(RegisterResponse {
        status: if purchase.activated_at.is_some() {
            "ACTIVE".to_string()
        } else {
            "PENDING".to_string()
        },
        name: purchase.name.clone(),
        lightning_address: lightning_address(&purchase.name, &state.domain),
        ark_address: purchase.ark_address.clone(),
        amount_msats: Some(purchase.fee_msats as u64),
        pr: Some(invoice.bolt11.clone()),
        payment_hash: Some(payment_hash.clone()),
        verify: Some(format!(
            "https://{}/v1/register/verify/{payment_hash}",
            state.domain
        )),
    })
}

fn registration_status_response(
    state: &State,
    invoice: &Invoice,
    purchase: &CustomAddressPurchase,
) -> RegisterStatusResponse {
    RegisterStatusResponse {
        status: "OK".to_string(),
        name: purchase.name.clone(),
        lightning_address: lightning_address(&purchase.name, &state.domain),
        ark_address: purchase.ark_address.clone(),
        amount_msats: purchase.fee_msats as u64,
        paid: invoice.state == InvoiceState::Settled as i32,
        active: purchase.activated_at.is_some(),
        pr: invoice.bolt11.clone(),
    }
}

fn invoice_payment_hash_string(invoice: &Invoice) -> String {
    invoice
        .payment_hash
        .clone()
        .unwrap_or_else(|| invoice.bolt11().payment_hash().to_string())
}

fn find_purchase_for_invoice(
    state: &State,
    invoice_id: i32,
) -> Result<CustomAddressPurchase, (StatusCode, Json<Value>)> {
    let mut conn = state.db_pool.get().map_err(|e| {
        error!("DB connection error: {e}");
        server_error_response()
    })?;

    CustomAddressPurchase::get_by_invoice_id(&mut conn, invoice_id)
        .map_err(|e| {
            error!("Error looking up custom address purchase for invoice={invoice_id}: {e:?}");
            server_error_response()
        })?
        .ok_or_else(|| (StatusCode::OK, Json(not_found_response())))
}

pub(crate) fn activate_custom_address_purchase(
    state: &State,
    invoice_id: i32,
) -> Result<Option<CustomAddressPurchase>, (StatusCode, Json<Value>)> {
    let mut conn = state.db_pool.get().map_err(|e| {
        error!("DB connection error: {e}");
        server_error_response()
    })?;

    CustomAddressPurchase::activate_for_invoice(&mut conn, invoice_id).map_err(|e| {
        error!("Error activating custom address purchase for invoice={invoice_id}: {e:?}");
        server_error_response()
    })
}

/// HTTP endpoint for verifying the status of a Lightning invoice payment.
///
/// This route is called by clients to check if an invoice has been paid.
///
/// # Parameters
/// * `desc_hash` and `pay_hash` - Path parameters for the description hash and payment hash
/// * `state` - Application state with LND client
///
/// # Returns
/// A JSON response indicating settlement status and preimage (if settled), or an error response
pub async fn verify(
    Path((desc_hash, pay_hash)): Path<(String, String)>,
    Extension(state): Extension<State>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    validate_hex_hash(&desc_hash, "Invalid description hash")?;
    validate_hex_hash(&pay_hash, "Invalid payment hash")?;

    let mut invoice = find_invoice_by_payment_hash(&state, &pay_hash)?;

    if invoice.state == InvoiceState::Pending as i32 {
        refresh_invoice_receive_status(&state, &invoice, &pay_hash).await?;
        invoice = find_invoice_by_payment_hash(&state, &pay_hash)?;
    }

    let bolt11 = invoice.bolt11();
    if !invoice_description_hash(&bolt11).eq_ignore_ascii_case(&desc_hash) {
        return Ok(Json(not_found_response()));
    }

    if invoice.state == InvoiceState::Settled as i32 && !invoice.preimage.is_empty() {
        Ok(Json(json!({
            "status": "OK",
            "settled": true,
            "preimage": invoice.preimage,
            "pr": bolt11,
        })))
    } else {
        Ok(Json(json!({
            "status": "OK",
            "settled": false,
            "preimage": null,
            "pr": bolt11,
        })))
    }
}

fn validate_hex_hash(hash: &str, reason: &str) -> Result<(), (StatusCode, Json<Value>)> {
    if hash.len() == 64 && hex::decode(hash).is_ok_and(|bytes| bytes.len() == 32) {
        Ok(())
    } else {
        Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "ERROR",
                "reason": reason,
            })),
        ))
    }
}

fn find_invoice_by_payment_hash(
    state: &State,
    payment_hash: &str,
) -> Result<Invoice, (StatusCode, Json<Value>)> {
    let mut conn = state.db_pool.get().map_err(|e| {
        error!("DB connection error: {e}");
        server_error_response()
    })?;

    Invoice::get_by_payment_hash(&mut conn, payment_hash)
        .map_err(|e| {
            error!("Error looking up invoice for payment_hash={payment_hash}: {e:?}");
            server_error_response()
        })?
        .ok_or_else(|| (StatusCode::OK, Json(not_found_response())))
}

async fn refresh_invoice_receive_status(
    state: &State,
    invoice: &Invoice,
    payment_hash: &str,
) -> Result<(), (StatusCode, Json<Value>)> {
    let receive = state
        .barkd
        .receive_status(payment_hash)
        .await
        .map_err(|e| {
            error!("Error refreshing receive status for payment_hash={payment_hash}: {e:#}");
            server_error_response()
        })?;

    let mut conn = state.db_pool.get().map_err(|e| {
        error!("DB connection error: {e}");
        server_error_response()
    })?;

    if let Some(receive) = receive {
        if receive.preimage_revealed_at.is_some() {
            invoice
                .mark_settled(&mut conn, receive.payment_preimage.to_string())
                .map_err(|e| {
                    error!("Error marking invoice settled for payment_hash={payment_hash}: {e:?}");
                    server_error_response()
                })?;
        } else if receive.finished_at.is_some() {
            invoice.mark_cancelled(&mut conn).map_err(|e| {
                error!("Error marking invoice cancelled for payment_hash={payment_hash}: {e:?}");
                server_error_response()
            })?;
        }
    } else if invoice_has_expired(invoice) {
        invoice.mark_cancelled(&mut conn).map_err(|e| {
            error!(
                "Error marking expired invoice cancelled for payment_hash={payment_hash}: {e:?}"
            );
            server_error_response()
        })?;
    }

    Ok(())
}

fn invoice_has_expired(invoice: &Invoice) -> bool {
    invoice
        .expires_at
        .is_some_and(|expires_at| expires_at <= chrono::Utc::now().naive_utc())
        || invoice.bolt11().is_expired()
}

fn not_found_response() -> Value {
    json!({
        "status": "ERROR",
        "reason": "Not found",
    })
}

fn server_error_response() -> (StatusCode, Json<Value>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "status": "ERROR",
            "reason": "Server error",
        })),
    )
}

/// Utility function for converting anyhow errors to HTTP response format.
///
/// # Parameters
/// * `err` - The anyhow Error to convert
///
/// # Returns
/// A tuple containing a 400 Bad Request status code and a JSON error response
pub(crate) fn handle_anyhow_error(err: anyhow::Error) -> (StatusCode, Json<Value>) {
    let err = json!({
        "status": "ERROR",
        "reason": format!("{err}"),
    });
    (StatusCode::BAD_REQUEST, Json(err))
}

/// Fallback route handler that returns a 404 Not Found response
/// when a request is made to a non-existent route.
///
/// # Parameters
/// * `uri` - The URI of the request
///
/// # Returns
/// A 404 status code and a message indicating the route was not found
pub async fn fallback(uri: Uri) -> (StatusCode, String) {
    (StatusCode::NOT_FOUND, format!("No route for {}", uri))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn metadata_matches_lnurl_identifier_format() {
        assert_eq!(
            calc_metadata("alice", "example.com"),
            "[[\"text/identifier\",\"alice@example.com\"],[\"text/plain\",\"Sats for alice\"]]"
        );
    }

    #[test]
    fn amount_validation_accepts_whole_sats_within_bounds() {
        validate_amount_msats(10_000, 1_000, 100_000).unwrap();
    }

    #[test]
    fn amount_validation_rejects_out_of_bounds_amounts() {
        assert_eq!(
            validate_amount_msats(999, 1_000, 100_000)
                .unwrap_err()
                .to_string(),
            "Amount out of bounds"
        );
        assert_eq!(
            validate_amount_msats(101_000, 1_000, 100_000)
                .unwrap_err()
                .to_string(),
            "Amount out of bounds"
        );
    }

    #[test]
    fn amount_validation_rejects_non_whole_sat_amounts() {
        assert_eq!(
            validate_amount_msats(1_001, 1_000, 100_000)
                .unwrap_err()
                .to_string(),
            "Bark invoices must be denominated in whole sats"
        );
    }

    #[test]
    fn name_validation_rejects_empty_long_or_invalid_names() {
        assert_eq!(
            validate_name("").unwrap_err().to_string(),
            "Name parameter is required"
        );
        assert_eq!(
            validate_name(&"a".repeat(MAX_NAME_LEN + 1))
                .unwrap_err()
                .to_string(),
            "Name parameter is too long"
        );
        assert_eq!(
            validate_name("alice/bob").unwrap_err().to_string(),
            "Name parameter contains invalid characters"
        );
    }

    #[test]
    fn callback_validation_rejects_oversized_inputs() {
        let params = LnurlCallbackParams {
            comment: Some("a".repeat(MAX_COMMENT_LEN + 1)),
            ..Default::default()
        };
        assert_eq!(
            validate_callback_params(&params).unwrap_err().to_string(),
            "Comment is too long"
        );

        let params = LnurlCallbackParams {
            nostr: Some("a".repeat(MAX_NOSTR_PARAM_LEN + 1)),
            ..Default::default()
        };
        assert_eq!(
            validate_callback_params(&params).unwrap_err().to_string(),
            "Nostr parameter is too large"
        );
    }

    #[test]
    fn empty_callback_strings_deserialize_to_none() {
        let params: LnurlCallbackParams = serde_json::from_value(json!({
            "amount": 1_000,
            "comment": "",
            "nostr": ""
        }))
        .unwrap();

        assert_eq!(params.amount, Some(1_000));
        assert_eq!(params.comment, None);
        assert_eq!(params.nostr, None);
    }
}

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: String,
    pub version: String,
}

impl HealthResponse {
    /// Fabricate a status: pass response without checking database connectivity
    pub fn new_ok() -> Self {
        Self {
            status: String::from("pass"),
            version: String::from("0"),
        }
    }
}

/// IETF draft RFC for HTTP API Health Checks:
/// https://datatracker.ietf.org/doc/html/draft-inadarei-api-health-check
pub async fn health_check() -> Result<Json<HealthResponse>, (StatusCode, String)> {
    Ok(Json(HealthResponse::new_ok()))
}

pub fn empty_string_as_none<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: FromStr,
    T::Err: Display,
{
    let opt = Option::<String>::deserialize(de)?;
    match opt.as_deref() {
        None | Some("") => Ok(None),
        Some(s) => FromStr::from_str(s).map_err(de::Error::custom).map(Some),
    }
}
