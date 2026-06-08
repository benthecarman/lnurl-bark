// @generated automatically by Diesel CLI.

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

diesel::joinable!(invoice -> users (user_id));
diesel::joinable!(zaps -> invoice (id));

diesel::allow_tables_to_appear_in_same_query!(invoice, users, zaps,);
