// @generated automatically by Diesel CLI.

diesel::table! {
    custom_address_purchases (invoice_id) {
        invoice_id -> Int4,
        user_id -> Int4,
        #[max_length = 255]
        name -> Varchar,
        ark_address -> Text,
        auth_message -> Text,
        #[max_length = 128]
        signature -> Varchar,
        fee_msats -> Int8,
        created_at -> Timestamp,
        activated_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    invoice (id) {
        id -> Int4,
        user_id -> Int4,
        #[max_length = 2048]
        bolt11 -> Varchar,
        amount_msats -> Int8,
        #[max_length = 64]
        payment_hash -> Nullable<Varchar>,
        #[max_length = 64]
        preimage -> Varchar,
        #[max_length = 100]
        lnurlp_comment -> Nullable<Varchar>,
        state -> Int4,
        created_at -> Timestamp,
        expires_at -> Nullable<Timestamp>,
        settled_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    users (id) {
        id -> Int4,
        ark_address -> Text,
        #[max_length = 255]
        name -> Varchar,
        disabled_zaps -> Bool,
        activated_at -> Nullable<Timestamp>,
    }
}

diesel::table! {
    zaps (id) {
        id -> Int4,
        request -> Text,
        #[max_length = 64]
        event_id -> Nullable<Varchar>,
    }
}

diesel::joinable!(custom_address_purchases -> invoice (invoice_id));
diesel::joinable!(custom_address_purchases -> users (user_id));
diesel::joinable!(invoice -> users (user_id));
diesel::joinable!(zaps -> invoice (id));

diesel::allow_tables_to_appear_in_same_query!(custom_address_purchases, invoice, users, zaps,);
