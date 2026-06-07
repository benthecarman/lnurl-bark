use crate::models::invoice::{InvoiceState, NewInvoice};
use crate::models::user::{NewUser, User};
use crate::models::zap::Zap;
use crate::State;
use anyhow::anyhow;
use axum::extract::{Path, Query};
use axum::http::{StatusCode, Uri};
use axum::{Extension, Json};
use bitcoin::Amount;
use diesel::Connection;
use lightning_invoice::Bolt11Invoice;
use lnurl::pay::PayResponse;
use lnurl::Tag;
use log::error;
use nostr::{Event, JsonUtil};
use serde::{de, Deserialize, Deserializer, Serialize};
use serde_json::{json, Value};
use std::fmt::Display;
use std::str::FromStr;

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
    if params.amount.is_none() {
        return Err(anyhow!("Missing amount parameter"));
    }
    let amount_msats = params.amount.unwrap();
    validate_amount_msats(amount_msats, state.min_sendable, state.max_sendable)?;

    let mut conn = state.db_pool.get()?;

    let user = User::get_by_name(&mut conn, name)?.ok_or(anyhow!("User not found"))?;
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
        .wallet
        .bolt11_invoice_for_address(
            Amount::from_sat(amount_msats / 1_000),
            ark_address,
            Some(_invoice_description),
        )
        .await?;

    if invoice.amount_milli_satoshis().is_none()
        || invoice.amount_milli_satoshis().unwrap() != amount_msats
    {
        return Err(anyhow!("Invoice amount mismatch"));
    }

    conn.transaction::<_, anyhow::Error, _>(|conn| {
        let invoice = NewInvoice {
            user_id: user.id,
            bolt11: invoice.to_string(),
            amount_msats: amount_msats as i64,
            preimage: String::new(),
            lnurlp_comment: params.comment,
            state: InvoiceState::Pending as i32,
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
    match get_invoice_impl(&state, &name, params).await {
        Ok(invoice) => {
            // let payment_hash = hex::encode(invoice.payment_hash().to_byte_array());
            // let verify_url = format!("https://{}/verify/{name}/{payment_hash}", state.domain);
            Ok(Json(json!({
                "status": "OK",
                "pr": invoice,
                // "verify": verify_url,
                "routes": [],
            })))
        }
        Err(e) => Err(handle_anyhow_error(e)),
    }
}

pub fn calc_metadata(name: &str, domain: &str) -> String {
    format!("[[\"text/identifier\",\"{name}@{domain}\"],[\"text/plain\",\"Sats for {name}\"]]",)
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
    if name.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "status": "ERROR",
                "reason": "Name parameter is required",
            })),
        ));
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

    if User::get_by_name(&mut conn, &name)
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
pub struct RegisterRequest {
    pub name: String,
    pub ark_address: String,
}

#[derive(Serialize)]
pub struct RegisterResponse {
    pub name: String,
}

pub async fn register(
    state: &State,
    req: RegisterRequest,
) -> Result<RegisterResponse, (StatusCode, String)> {
    let mut conn = state.db_pool.get().map_err(|e| {
        error!("DB connection error: {e}");
        (StatusCode::INTERNAL_SERVER_ERROR, "ServerError".to_string())
    })?;

    let ark_address = req
        .ark_address
        .parse::<bark::ark::Address>()
        .map_err(|_| (StatusCode::BAD_REQUEST, "InvalidArkAddress".to_string()))?;

    // check if the user provided name is taken
    match User::get_by_name(&mut conn, &req.name) {
        Ok(Some(_)) => {
            return Err((StatusCode::BAD_REQUEST, "NameTaken".to_string()));
        }
        Ok(None) => (),
        Err(e) => {
            error!("Error checking name availability: {e:?}");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "ServerError".to_string()));
        }
    }

    let new_user = NewUser {
        ark_address: ark_address.to_string(),
        name: req.name,
    };
    match new_user.insert(&mut conn) {
        Ok(u) => Ok(RegisterResponse { name: u.name }),
        Err(e) => {
            error!("Error inserting new user: {e:?}");
            Err((StatusCode::INTERNAL_SERVER_ERROR, "ServerError".to_string()))
        }
    }
}

pub async fn register_route(
    Extension(state): Extension<State>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<RegisterResponse>, (StatusCode, String)> {
    let res = register(&state, req).await?;
    Ok(Json(res))
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
    Path((_desc_hash, _pay_hash)): Path<(String, String)>,
    Extension(_state): Extension<State>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    // todo implement
    Err((
        StatusCode::BAD_REQUEST,
        Json(json!({
            "status": "ERROR",
            "reason": "Invalid payment hash",
        })),
    ))

    // let mut lnd = state.lnd.clone();
    //
    // let desc_hash: Vec<u8> = hex::decode(desc_hash).map_err(|_| {
    //     (
    //         StatusCode::BAD_REQUEST,
    //         Json(json!({
    //             "status": "ERROR",
    //             "reason": "Invalid description hash",
    //         })),
    //     )
    // })?;
    //
    // let pay_hash: Vec<u8> = hex::decode(pay_hash).map_err(|_| {
    //     (
    //         StatusCode::BAD_REQUEST,
    //         Json(json!({
    //             "status": "ERROR",
    //             "reason": "Invalid payment hash",
    //         })),
    //     )
    // })?;
    //
    // let request = lnrpc::PaymentHash {
    //     r_hash: pay_hash.to_vec(),
    //     ..Default::default()
    // };
    //
    // let resp = match lnd.lookup_invoice(request).await {
    //     Ok(resp) => resp.into_inner(),
    //     Err(_) => {
    //         return Ok(Json(json!({
    //             "status": "ERROR",
    //             "reason": "Not found",
    //         })));
    //     }
    // };
    //
    // let invoice = Bolt11Invoice::from_str(&resp.payment_request).map_err(|_| {
    //     (
    //         StatusCode::OK,
    //         Json(json!({
    //             "status": "ERROR",
    //             "reason": "Not found",
    //         })),
    //     )
    // })?;
    //
    // match invoice.description() {
    //     Bolt11InvoiceDescriptionRef::Direct(_) => Ok(Json(json!({
    //         "status": "ERROR",
    //         "reason": "Not found",
    //     }))),
    //     Bolt11InvoiceDescriptionRef::Hash(h) => {
    //         if h.0.to_byte_array().to_vec() == desc_hash {
    //             if resp.state() == InvoiceState::Settled && !resp.r_preimage.is_empty() {
    //                 let preimage = hex::encode(resp.r_preimage);
    //                 Ok(Json(json!({
    //                     "status": "OK",
    //                     "settled": true,
    //                     "preimage": preimage,
    //                     "pr": invoice,
    //                 })))
    //             } else {
    //                 Ok(Json(json!({
    //                     "status": "OK",
    //                     "settled": false,
    //                     "preimage": (),
    //                     "pr": invoice,
    //                 })))
    //             }
    //         } else {
    //             Ok(Json(json!({
    //                 "status": "ERROR",
    //                 "reason": "Not found",
    //             })))
    //         }
    //     }
    // }
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
