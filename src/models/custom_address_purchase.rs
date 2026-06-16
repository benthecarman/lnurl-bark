use crate::models::schema::{custom_address_purchases, users};
use chrono::NaiveDateTime;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Queryable, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[diesel(check_for_backend(diesel::pg::Pg))]
#[diesel(table_name = custom_address_purchases)]
pub struct CustomAddressPurchase {
    pub invoice_id: i32,
    pub user_id: i32,
    pub name: String,
    pub ark_address: String,
    pub auth_message: String,
    pub signature: String,
    pub fee_msats: i64,
    pub created_at: NaiveDateTime,
    pub activated_at: Option<NaiveDateTime>,
}

impl CustomAddressPurchase {
    pub fn get_by_invoice_id(
        conn: &mut PgConnection,
        invoice_id: i32,
    ) -> anyhow::Result<Option<Self>> {
        Ok(custom_address_purchases::table
            .filter(custom_address_purchases::invoice_id.eq(invoice_id))
            .first::<Self>(conn)
            .optional()?)
    }

    pub fn get_by_user_id(conn: &mut PgConnection, user_id: i32) -> anyhow::Result<Option<Self>> {
        Ok(custom_address_purchases::table
            .filter(custom_address_purchases::user_id.eq(user_id))
            .first::<Self>(conn)
            .optional()?)
    }

    pub fn activate_for_invoice(
        conn: &mut PgConnection,
        invoice_id: i32,
    ) -> anyhow::Result<Option<Self>> {
        let Some(purchase) = Self::get_by_invoice_id(conn, invoice_id)? else {
            return Ok(None);
        };

        diesel::update(users::table)
            .filter(users::id.eq(purchase.user_id))
            .filter(users::activated_at.is_null())
            .set(users::activated_at.eq(diesel::dsl::now))
            .execute(conn)?;

        diesel::update(custom_address_purchases::table)
            .filter(custom_address_purchases::invoice_id.eq(invoice_id))
            .filter(custom_address_purchases::activated_at.is_null())
            .set(custom_address_purchases::activated_at.eq(diesel::dsl::now))
            .execute(conn)?;

        Self::get_by_invoice_id(conn, invoice_id)
    }

    pub fn replace_invoice_for_user(
        conn: &mut PgConnection,
        user_id: i32,
        invoice_id: i32,
        auth_message: String,
        signature: String,
        fee_msats: i64,
    ) -> anyhow::Result<Self> {
        diesel::update(custom_address_purchases::table)
            .filter(custom_address_purchases::user_id.eq(user_id))
            .set((
                custom_address_purchases::invoice_id.eq(invoice_id),
                custom_address_purchases::auth_message.eq(auth_message),
                custom_address_purchases::signature.eq(signature),
                custom_address_purchases::fee_msats.eq(fee_msats),
                custom_address_purchases::created_at.eq(diesel::dsl::now),
                custom_address_purchases::activated_at.eq(None::<NaiveDateTime>),
            ))
            .get_result::<Self>(conn)
            .map_err(|e| e.into())
    }
}

#[derive(Insertable)]
#[diesel(table_name = custom_address_purchases)]
pub struct NewCustomAddressPurchase {
    pub invoice_id: i32,
    pub user_id: i32,
    pub name: String,
    pub ark_address: String,
    pub auth_message: String,
    pub signature: String,
    pub fee_msats: i64,
}

impl NewCustomAddressPurchase {
    pub fn insert(&self, conn: &mut PgConnection) -> anyhow::Result<CustomAddressPurchase> {
        diesel::insert_into(custom_address_purchases::table)
            .values(self)
            .get_result::<CustomAddressPurchase>(conn)
            .map_err(|e| e.into())
    }
}
