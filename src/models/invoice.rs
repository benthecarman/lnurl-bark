use crate::models::schema::invoice;
use chrono::NaiveDateTime;
use diesel::prelude::*;
use lightning_invoice::Bolt11Invoice;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(
    QueryableByName, Queryable, AsChangeset, Serialize, Deserialize, Debug, Clone, PartialEq,
)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[diesel(table_name = invoice)]
pub struct Invoice {
    pub id: i32,
    pub user_id: i32,
    pub bolt11: String,
    pub amount_msats: i64,
    pub payment_hash: Option<String>,
    pub preimage: String,
    pub lnurlp_comment: Option<String>,
    pub state: i32,
    pub created_at: NaiveDateTime,
    pub expires_at: Option<NaiveDateTime>,
    pub settled_at: Option<NaiveDateTime>,
}

impl Invoice {
    pub fn bolt11(&self) -> Bolt11Invoice {
        Bolt11Invoice::from_str(&self.bolt11).expect("invalid bolt11")
    }

    pub fn get_invoices(conn: &mut PgConnection) -> anyhow::Result<Vec<Invoice>> {
        Ok(invoice::table.load::<Self>(conn)?)
    }

    pub fn get_by_id(conn: &mut PgConnection, user_id: i32) -> anyhow::Result<Option<Invoice>> {
        Ok(invoice::table
            .filter(invoice::id.eq(user_id))
            .first::<Invoice>(conn)
            .optional()?)
    }

    pub fn get_by_payment_hash(
        conn: &mut PgConnection,
        payment_hash: &str,
    ) -> anyhow::Result<Option<Invoice>> {
        Ok(invoice::table
            .filter(invoice::payment_hash.eq(payment_hash))
            .first::<Invoice>(conn)
            .optional()?)
    }

    pub fn get_by_state(conn: &mut PgConnection, state: i32) -> anyhow::Result<Vec<Invoice>> {
        Ok(invoice::table
            .filter(invoice::state.eq(state))
            .order(invoice::id.asc())
            .load::<Invoice>(conn)?)
    }

    pub fn cancel_expired_pending(conn: &mut PgConnection) -> anyhow::Result<usize> {
        Ok(diesel::update(invoice::table)
            .filter(invoice::state.eq(InvoiceState::Pending as i32))
            .filter(invoice::expires_at.le(diesel::dsl::now))
            .set(invoice::state.eq(InvoiceState::Cancelled as i32))
            .execute(conn)?)
    }

    pub fn mark_settled(&self, conn: &mut PgConnection, preimage: String) -> anyhow::Result<bool> {
        let updated = diesel::update(invoice::table)
            .filter(invoice::id.eq(self.id))
            .filter(invoice::state.eq(InvoiceState::Pending as i32))
            .set((
                invoice::state.eq(InvoiceState::Settled as i32),
                invoice::preimage.eq(preimage),
                invoice::settled_at.eq(diesel::dsl::now),
            ))
            .execute(conn)?;

        Ok(updated == 1)
    }

    pub fn mark_cancelled(&self, conn: &mut PgConnection) -> anyhow::Result<bool> {
        let updated = diesel::update(invoice::table)
            .filter(invoice::id.eq(self.id))
            .filter(invoice::state.eq(InvoiceState::Pending as i32))
            .set(invoice::state.eq(InvoiceState::Cancelled as i32))
            .execute(conn)?;

        Ok(updated == 1)
    }
}

#[derive(Insertable)]
#[diesel(table_name = invoice)]
pub struct NewInvoice {
    pub user_id: i32,
    pub bolt11: String,
    pub amount_msats: i64,
    pub payment_hash: Option<String>,
    pub preimage: String,
    pub lnurlp_comment: Option<String>,
    pub state: i32,
    pub expires_at: Option<NaiveDateTime>,
}

impl NewInvoice {
    pub fn insert(&self, conn: &mut PgConnection) -> anyhow::Result<Invoice> {
        diesel::insert_into(invoice::table)
            .values(self)
            .get_result::<Invoice>(conn)
            .map_err(|e| e.into())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum InvoiceState {
    /// The invoice is pending payment.
    Pending = 0,
    /// The invoice has been paid and settled.
    Settled = 1,
    /// The invoice has been cancelled or expired.
    Cancelled = 2,
}
