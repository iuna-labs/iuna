use std::{collections::BTreeMap, sync::Arc};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Method, Request, StatusCode, header},
    middleware,
    routing::{get, post},
};
use tokio::sync::Mutex;
use tower::ServiceExt;

use crate::{
    adapters::{
        chain_store::SqliteChainStore, config_store, config_store::UiConfig, p2p::GossipNetwork,
        wallet_store,
    },
    app::{GossipEnvelope, NodeCore, PeerBook, PeerDirection, PeerInfo, StratumStatus},
    domain::{
        Amount, BlindedReveal, BlindedTransaction, Block, ChainSnapshot, GenesisBurn,
        LaunchProfile, Ledger, MICRO_IUNA, MINE_FINALIZER_FEE, MaskedBlindedReveal, OutPoint,
        RevealBundleSection, RevealBundleSignature, Transaction, TxInput, TxOutput, Wallet,
    },
};

use super::{
    AUTH_COOKIE_NAME, HttpState, PEER_STALE_AFTER_MS, TransferForm, WalletTransactionFilters,
    WalletTransactionsQuery, api_auth_change_password_form, api_auth_login_form,
    api_auth_setup_form, api_auth_status, auth_client_key, dev_seed_verify_bypass_allowed,
    hash_password, hex_encode, pbkdf2_sha256, persist_burn_settings_config,
    persist_pow_mining_config, require_auth_middleware, required_fee_per_byte_burn,
    same_origin_request, validate_password, validate_transfer_form, verify_password,
    wallet_transaction_rows, wallet_utxo_rows,
};

#[test]
fn dev_seed_verify_bypass_requires_env_flag() {
    assert!(dev_seed_verify_bypass_allowed(true));
    assert!(!dev_seed_verify_bypass_allowed(false));
}

#[test]
fn mempool_ui_items_can_represent_blinded_transactions_and_reveals() {
    let commitment = "a".repeat(64);
    let owner = "c".repeat(64);
    let input_outpoint = OutPoint {
        txid: "d".repeat(64),
        index: 0,
    };
    let blinded = BlindedTransaction {
        commitment: commitment.clone(),
        inputs: vec![TxInput {
            outpoint: input_outpoint.clone(),
            owner: owner.clone(),
            signature: "e".repeat(128),
        }],
        fee: 7,
        encrypted_size: 123,
        expires_at_height: 42,
        nonce: "00".repeat(12),
        ciphertext: "11".repeat(123),
        payload_hash: "b".repeat(64),
    };
    let reveal = BlindedReveal {
        commitment: commitment.clone(),
        key: "22".repeat(32),
    };
    let outputs = BTreeMap::from([(
        input_outpoint.clone(),
        TxOutput {
            address: owner.clone(),
            amount: 99,
        },
    )]);

    let blinded_row = super::ui_blinded_transaction(&blinded, &outputs);
    let reveal_row = super::ui_blinded_reveal(&reveal);
    let revealed_row = super::ui_pending_revealed_transaction(
        &crate::domain::RevealedBlindedTransaction {
            height: 2,
            commitment: reveal.commitment.clone(),
            included_by: owner.clone(),
            transaction: Transaction::Burn {
                inputs: blinded.inputs.clone(),
                change: Vec::new(),
                amount: 12,
                fee: blinded.fee,
                signature: "f".repeat(128),
            },
        },
        &outputs,
    );

    assert_eq!(blinded_row.kind, "blinded");
    assert_eq!(blinded_row.from, owner);
    assert_eq!(blinded_row.inputs.len(), 1);
    assert_eq!(blinded_row.inputs[0].amount, Some(99));
    assert_eq!(blinded_row.signature, commitment);
    assert_eq!(blinded_row.encrypted_size, Some(123));
    assert_eq!(blinded_row.expires_at_height, Some(42));
    assert_eq!(reveal_row.kind, "reveal");
    assert_eq!(reveal_row.commitment, blinded_row.commitment);
    assert_eq!(revealed_row.kind, "burn");
    assert!(revealed_row.revealed);
    assert_eq!(revealed_row.commitment, Some(reveal.commitment));
    assert_eq!(revealed_row.amount, 12);
    assert_eq!(revealed_row.fee, 7);
    assert_eq!(revealed_row.inputs[0].amount, Some(99));
}

#[test]
fn block_detail_transactions_include_blinded_and_revealed_items() {
    let alice = Wallet::from_seed("block-detail-blind-alice");
    let bob = Wallet::from_seed("block-detail-blind-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 100);
    allocations.insert(bob.address().to_string(), 100);
    let ledger = Ledger::new(allocations.clone(), 1);
    let transfer = ledger.build_transfer(&alice, bob.address(), 12, 3).unwrap();
    let built = ledger
        .build_blinded_transaction(&alice, transfer.clone(), 20)
        .unwrap();
    let mut block = fake_block(7, Vec::new());
    block.blinded_transactions = vec![built.transaction.clone()];

    let revealed = crate::domain::RevealedBlindedTransaction {
        height: 7,
        commitment: built.transaction.commitment.clone(),
        included_by: "miner".to_string(),
        transaction: transfer,
    };
    let ui_block = super::ui_block(block, &BTreeMap::new(), &BTreeMap::new(), &[revealed]);

    assert_eq!(ui_block.transactions.len(), 2);
    assert_eq!(ui_block.transactions[0].kind, "blinded");
    assert_eq!(
        ui_block.transactions[0].commitment.as_deref(),
        Some(built.transaction.commitment.as_str())
    );
    assert!(!ui_block.transactions[0].revealed);
    assert_eq!(ui_block.transactions[1].kind, "transfer");
    assert!(ui_block.transactions[1].revealed);
    assert_eq!(ui_block.revealed_transactions.len(), 1);
    assert!(ui_block.revealed_transactions[0].revealed);
    assert!(ui_block.total_bytes > 0);
    assert!(ui_block.blinded_transaction_bytes > 0);
    assert_eq!(ui_block.reveal_bundle_bytes, 0);

    let burn = ledger.build_burn(&alice, 5, 1).unwrap();
    let mine = Transaction::Mine {
        recipient: bob.address().to_string(),
        anchor: "a".repeat(64),
        salt: 1,
        nonce: 2,
        difficulty_bits: 12,
        proof_header: None,
        signature: "b".repeat(64),
    };
    let typed_block = super::ui_block(
        fake_block(
            8,
            vec![
                ledger.build_transfer(&alice, bob.address(), 7, 1).unwrap(),
                burn,
                mine,
            ],
        ),
        &BTreeMap::new(),
        &BTreeMap::new(),
        &[],
    );
    let typed_bytes = typed_block
        .transaction_byte_breakdown
        .iter()
        .map(|row| (row.label, row.bytes))
        .collect::<BTreeMap<_, _>>();
    assert!(typed_bytes["transfer"] > 0);
    assert!(typed_bytes["burn"] > 0);
    assert!(typed_bytes["mine"] > 0);
}

#[test]
fn block_detail_reconstructs_revealed_items_from_snapshot_blocks() {
    let alice = Wallet::from_seed("block-detail-reveal-alice");
    let bob = Wallet::from_seed("block-detail-reveal-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 100);
    allocations.insert(bob.address().to_string(), 100);
    let ledger = Ledger::new(allocations.clone(), 1);
    let transfer = ledger.build_transfer(&alice, bob.address(), 12, 3).unwrap();
    let built = ledger
        .build_blinded_transaction(&alice, transfer.clone(), 20)
        .unwrap();
    let mut commit_block = fake_block(7, Vec::new());
    commit_block.blinded_transactions = vec![built.transaction.clone()];
    let mut reveal_block = fake_block(8, Vec::new());
    reveal_block.reveal_bundle_section = RevealBundleSection {
        signatures: vec![RevealBundleSignature {
            slot: 0,
            member: reveal_block.miner.clone(),
            signature: "11".repeat(64),
        }],
        reveals: vec![MaskedBlindedReveal {
            reveal: built.reveal.clone(),
            bundle_mask: 1,
        }],
    };
    let snapshot = fake_snapshot(
        allocations,
        vec![commit_block.clone(), reveal_block.clone()],
    );

    let blocks = super::ui_blocks(
        vec![commit_block, reveal_block],
        &snapshot,
        &[],
        &BTreeMap::new(),
    );

    assert_eq!(blocks[0].transactions.len(), 1);
    assert_eq!(blocks[0].transactions[0].kind, "blinded");
    assert_eq!(
        blocks[0].transactions[0].commitment.as_deref(),
        Some(built.transaction.commitment.as_str())
    );
    assert_eq!(blocks[1].transactions.len(), 1);
    assert_eq!(blocks[1].transactions[0].kind, "transfer");
    assert!(blocks[1].transactions[0].revealed);
    assert_eq!(blocks[1].transactions[0].amount, transfer.amount());
    assert_eq!(blocks[1].transactions[0].to.as_deref(), Some(bob.address()));
    assert_eq!(blocks[1].revealed_transactions.len(), 1);
}

#[test]
fn block_detail_markup_uses_blinded_and_revealed_labels() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");

    assert!(super::INDEX_HTML.contains("txPillLabel(tx)"));
    assert!(super::INDEX_HTML.contains("commitCountLabel(block)"));
    assert!(super::INDEX_HTML.contains(".pill.blinded"));
    assert!(super::INDEX_HTML.contains(".pill.reveal, .pill.revealed"));
    assert!(super::INDEX_HTML.contains(":class=\"mempoolItemClass(tx)\""));
    assert!(super::INDEX_HTML.contains(".mempool-item.before-last-block"));
    assert!(super::INDEX_HTML.contains(":class=\"row[2]\""));
    assert!(super::INDEX_HTML.contains("New since last block"));
    assert!(super::INDEX_HTML.contains("class=\"mempool-top\""));
    assert!(super::INDEX_HTML.contains("mempoolSeenTimeLabel(tx)"));
    assert!(super::INDEX_HTML.contains("<details class=\"tx-section\">"));
    assert!(super::INDEX_HTML.contains("<summary class=\"tx-section-title\">"));
    assert!(super::INDEX_HTML.contains("Commitment"));
    assert!(!super::INDEX_HTML.contains("<h3>Revealed</h3>"));
    assert!(app_js.contains("tx?.revealed ? \"revealed\""));
    assert!(app_js.contains("transactions.some((tx) => tx?.revealed)"));
    assert!(app_js.contains("blockCommitCount(block)"));
    assert!(app_js.contains("blockTransactionByteBreakdown(block)"));
    assert!(app_js.contains("[label, Number(row.bytes ?? 0), label]"));
}

#[test]
fn password_policy_rejects_short_or_excessive_passwords() {
    let short = validate_password("too-short").unwrap_err();
    assert!(short.to_string().contains("at least 12"));

    let long_password = "x".repeat(1025);
    let long = validate_password(&long_password).unwrap_err();
    assert!(long.to_string().contains("too long"));

    validate_password("correct horse battery staple").unwrap();
}

#[test]
fn password_hash_round_trips_without_storing_plaintext() {
    let password = "correct horse battery staple";
    let encoded = hash_password(password).unwrap();

    assert!(!encoded.contains(password));
    assert!(verify_password(password, &encoded).unwrap());
    assert!(!verify_password("wrong horse battery staple", &encoded).unwrap());
}

#[test]
fn pbkdf2_sha256_matches_known_vectors() {
    let one_iteration = pbkdf2_sha256(b"password", b"salt", 1);
    assert_eq!(
        hex_encode(one_iteration),
        "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
    );

    let two_iterations = pbkdf2_sha256(b"password", b"salt", 2);
    assert_eq!(
        hex_encode(two_iterations),
        "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
    );
}

#[test]
fn same_origin_check_accepts_forwarded_host_and_rejects_cross_site_origin() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "127.0.0.1:18661".parse().unwrap());
    headers.insert("x-forwarded-host", "iuna.example".parse().unwrap());
    headers.insert(header::ORIGIN, "https://iuna.example".parse().unwrap());
    assert!(same_origin_request(&headers));

    headers.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
    assert!(!same_origin_request(&headers));
}

#[test]
fn auth_client_key_trusts_forwarded_headers_only_from_private_or_local_peers() {
    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", "198.51.100.99".parse().unwrap());
    let socket = Some("203.0.113.10:51234".parse().unwrap());

    assert_eq!(auth_client_key(&headers, socket), "203.0.113.10");
    assert_eq!(
        auth_client_key(&headers, Some("127.0.0.1:51234".parse().unwrap())),
        "198.51.100.99"
    );
    assert_eq!(
        auth_client_key(&headers, Some("10.42.1.12:51234".parse().unwrap())),
        "198.51.100.99"
    );
    assert_eq!(
        auth_client_key(&headers, Some("172.20.4.8:51234".parse().unwrap())),
        "198.51.100.99"
    );
    assert_eq!(auth_client_key(&headers, None), "198.51.100.99");
}

#[tokio::test]
async fn protected_endpoints_require_authentication_setup() {
    let dir = tempfile::tempdir().unwrap();
    let state = auth_test_state(
        dir.path().join("config.json"),
        UiConfig {
            auth_password_hash: None,
            ..UiConfig::default()
        },
    )
    .await;
    let app = auth_test_app(state);

    let protected = http_request(app.clone(), Method::GET, "/api/protected", None, "").await;
    assert_eq!(protected.status, StatusCode::UNAUTHORIZED);
    assert!(protected.body.contains("authentication setup is required"));

    let status = http_request(app, Method::GET, "/api/auth/status", None, "").await;
    assert_eq!(status.status, StatusCode::OK);
    assert!(status.body.contains("\"configured\":false"));
}

#[tokio::test]
async fn favicon_is_public_before_authentication_setup() {
    let dir = tempfile::tempdir().unwrap();
    let state = auth_test_state(
        dir.path().join("config.json"),
        UiConfig {
            auth_password_hash: None,
            ..UiConfig::default()
        },
    )
    .await;
    let app = auth_test_app(state);

    let response = http_request(app, Method::GET, "/favicon.ico", None, "").await;

    assert_eq!(response.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn protected_endpoints_require_valid_session_after_authentication_setup() {
    let dir = tempfile::tempdir().unwrap();
    let password = "correct horse battery staple";
    let state = auth_test_state(
        dir.path().join("config.json"),
        UiConfig {
            auth_password_hash: Some(hash_password(password).unwrap()),
            ..UiConfig::default()
        },
    )
    .await;
    let app = auth_test_app(state);

    let missing_cookie = http_request(app.clone(), Method::GET, "/api/protected", None, "").await;
    assert_eq!(missing_cookie.status, StatusCode::UNAUTHORIZED);
    assert!(missing_cookie.body.contains("authentication required"));

    let bad_cookie = http_request(
        app.clone(),
        Method::GET,
        "/api/protected",
        Some("iuna_session=bogus"),
        "",
    )
    .await;
    assert_eq!(bad_cookie.status, StatusCode::UNAUTHORIZED);

    let login = http_request(
        app.clone(),
        Method::POST,
        "/api/auth/login",
        None,
        "password=correct+horse+battery+staple",
    )
    .await;
    assert_eq!(login.status, StatusCode::OK);
    assert!(login.body.contains("\"ok\":true"));
    let cookie = set_cookie_pair(&login.headers);
    assert!(cookie.starts_with(AUTH_COOKIE_NAME));

    let protected = http_request(app, Method::GET, "/api/protected", Some(&cookie), "").await;
    assert_eq!(protected.status, StatusCode::OK);
    assert_eq!(protected.body, "protected");
}

#[tokio::test]
async fn auth_posts_require_same_origin_headers() {
    let dir = tempfile::tempdir().unwrap();
    let password = "correct horse battery staple";
    let state = auth_test_state(
        dir.path().join("config.json"),
        UiConfig {
            auth_password_hash: Some(hash_password(password).unwrap()),
            ..UiConfig::default()
        },
    )
    .await;
    let app = auth_test_app(state);

    let response = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/api/auth/login")
                .header(header::HOST, "127.0.0.1:18661")
                .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from("password=correct+horse+battery+staple"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    assert!(body.contains("same-origin request required"));
}

#[tokio::test]
async fn login_authentication_locks_out_after_repeated_failures() {
    let dir = tempfile::tempdir().unwrap();
    let password = "correct horse battery staple";
    let state = auth_test_state(
        dir.path().join("config.json"),
        UiConfig {
            auth_password_hash: Some(hash_password(password).unwrap()),
            ..UiConfig::default()
        },
    )
    .await;
    let client_a = "198.51.100.10";
    let client_b = "198.51.100.11";

    for _ in 0..super::AUTH_MAX_FAILED_ATTEMPTS {
        let error = super::login_auth_password(&state, "wrong horse battery staple", client_a)
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("invalid password"));
    }

    let locked = super::login_auth_password(&state, password, client_a)
        .await
        .unwrap_err();
    assert!(format!("{locked:#}").contains("too many failed login attempts"));

    let other_client_cookie = super::login_auth_password(&state, password, client_b)
        .await
        .unwrap();
    assert!(other_client_cookie.starts_with(AUTH_COOKIE_NAME));

    state
        .auth_backoff
        .lock()
        .await
        .get_mut(client_a)
        .unwrap()
        .locked_until_ms = Some(crate::app::now_ms().saturating_sub(1));
    let cookie = super::login_auth_password(&state, password, client_a)
        .await
        .unwrap();
    assert!(cookie.starts_with(AUTH_COOKIE_NAME));
    assert!(!state.auth_backoff.lock().await.contains_key(client_a));
}

#[tokio::test]
async fn password_setup_creates_session_for_protected_endpoints() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let state = auth_test_state(config_path.clone(), UiConfig::default()).await;
    let app = auth_test_app(state);

    let setup = http_request(
        app.clone(),
        Method::POST,
        "/api/auth/setup",
        None,
        "password=correct+horse+battery+staple",
    )
    .await;
    assert_eq!(setup.status, StatusCode::OK);
    assert!(setup.body.contains("\"ok\":true"));
    let cookie = set_cookie_pair(&setup.headers);

    let stored = config_store::load_or_create(&config_path).unwrap();
    assert!(stored.auth_password_hash.is_some());
    let protected = http_request(app, Method::GET, "/api/protected", Some(&cookie), "").await;
    assert_eq!(protected.status, StatusCode::OK);
    assert_eq!(protected.body, "protected");
}

#[tokio::test]
async fn password_change_reencrypts_wallet_and_replaces_login_password() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let wallet_path = config_path.with_file_name("wallet.json");
    let old_password = "correct horse battery staple";
    let new_password = "new correct battery staple";
    let state = auth_test_state(
        config_path.clone(),
        UiConfig {
            auth_password_hash: Some(hash_password(old_password).unwrap()),
            setup_complete: true,
            ..UiConfig::default()
        },
    )
    .await;
    wallet_store::encrypt_existing_with_password(&wallet_path, old_password).unwrap();
    let app = auth_test_app(state);

    let login = http_request(
        app.clone(),
        Method::POST,
        "/api/auth/login",
        None,
        "password=correct+horse+battery+staple",
    )
    .await;
    assert_eq!(login.status, StatusCode::OK);
    let cookie = set_cookie_pair(&login.headers);

    let change = http_request(
        app.clone(),
        Method::POST,
        "/api/auth/change-password",
        Some(&cookie),
        "old_password=correct+horse+battery+staple&new_password=new+correct+battery+staple",
    )
    .await;
    assert_eq!(change.status, StatusCode::OK);
    assert!(change.body.contains("\"ok\":true"));

    let stored = config_store::load_or_create(&config_path).unwrap();
    let stored_hash = stored.auth_password_hash.unwrap();
    assert!(!verify_password(old_password, &stored_hash).unwrap());
    assert!(verify_password(new_password, &stored_hash).unwrap());
    assert!(wallet_store::load_with_password(&wallet_path, old_password).is_err());
    assert!(wallet_store::load_with_password(&wallet_path, new_password).is_ok());

    let old_login = http_request(
        app.clone(),
        Method::POST,
        "/api/auth/login",
        None,
        "password=correct+horse+battery+staple",
    )
    .await;
    assert!(old_login.body.contains("invalid password"));

    let new_login = http_request(
        app,
        Method::POST,
        "/api/auth/login",
        None,
        "password=new+correct+battery+staple",
    )
    .await;
    assert_eq!(new_login.status, StatusCode::OK);
}

#[tokio::test]
async fn peer_management_updates_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let state = auth_test_state(
        config_path.clone(),
        UiConfig {
            setup_complete: true,
            ..UiConfig::default()
        },
    )
    .await;

    super::add_peer(&state, " 127.0.0.1:9445 ".to_string())
        .await
        .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert_eq!(config.peers, vec!["127.0.0.1:9445"]);
    assert_eq!(state.peers.lock().await.addresses(), vec!["127.0.0.1:9445"]);

    super::remove_peer(&state, "127.0.0.1:9445".to_string())
        .await
        .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert!(config.peers.is_empty());
    assert!(state.peers.lock().await.addresses().is_empty());
}

#[tokio::test]
async fn address_book_updates_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let state = auth_test_state(
        config_path.clone(),
        UiConfig {
            setup_complete: true,
            ..UiConfig::default()
        },
    )
    .await;
    let alice = Wallet::from_seed("address-book-alice");
    let bob = Wallet::from_seed("address-book-bob");

    super::upsert_address_book_entry(
        &state,
        format!(" {} ", alice.address().to_ascii_uppercase()),
        " Alice ".to_string(),
        None,
    )
    .await
    .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert_eq!(
        config.address_book.get(alice.address()),
        Some(&"Alice".to_string())
    );

    super::upsert_address_book_entry(
        &state,
        alice.address().to_string(),
        "Alice Prime".to_string(),
        Some(alice.address().to_string()),
    )
    .await
    .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert_eq!(
        config.address_book.get(alice.address()),
        Some(&"Alice Prime".to_string())
    );

    let error = super::upsert_address_book_entry(
        &state,
        alice.address().to_string(),
        "Alice Duplicate".to_string(),
        None,
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("address is already saved"));

    let carol = Wallet::from_seed("address-book-carol");
    super::upsert_address_book_entry(
        &state,
        carol.address().to_string(),
        "Carol".to_string(),
        None,
    )
    .await
    .unwrap();
    let error = super::upsert_address_book_entry(
        &state,
        carol.address().to_string(),
        "Bob As Carol".to_string(),
        Some(alice.address().to_string()),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("address is already saved"));

    super::upsert_address_book_entry(
        &state,
        bob.address().to_string(),
        "Bob".to_string(),
        Some(alice.address().to_string()),
    )
    .await
    .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert!(!config.address_book.contains_key(alice.address()));
    assert_eq!(
        config.address_book.get(bob.address()),
        Some(&"Bob".to_string())
    );
    assert_eq!(
        config.address_book.get(carol.address()),
        Some(&"Carol".to_string())
    );

    let error = super::upsert_address_book_entry(
        &state,
        "iuna-address".to_string(),
        "Not Alice".to_string(),
        None,
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("invalid address book address"));

    super::remove_address_book_entry(&state, bob.address().to_string())
        .await
        .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert!(!config.address_book.contains_key(bob.address()));
    assert!(config.address_book.contains_key(carol.address()));
}

#[tokio::test]
async fn p2p_announce_setting_persists_config_and_waits_for_public_node() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let state = auth_test_state(
        config_path.clone(),
        UiConfig {
            setup_complete: true,
            ..UiConfig::default()
        },
    )
    .await;

    super::set_p2p_announce_addr(&state, " 203.0.113.10:9444 ".to_string())
        .await
        .unwrap();

    let config = config_store::load_or_create(&config_path).unwrap();
    assert_eq!(
        config.p2p_announce_addr.as_deref(),
        Some("203.0.113.10:9444")
    );
    match state.gossip.peer_exchange().await {
        GossipEnvelope::PeerList { peers } => {
            assert!(!peers.contains(&"203.0.113.10:9444".to_string()));
        }
        other => panic!("expected peer list, got {other:?}"),
    }

    super::set_p2p_accept_inbound(&state, true, Some(9555))
        .await
        .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert!(config.p2p_accept_inbound);
    assert_eq!(config.p2p_bind_port, 9555);
    assert!(!state.gossip.accepts_inbound().await);
    match state.gossip.peer_exchange().await {
        GossipEnvelope::PeerList { peers } => {
            assert!(!peers.contains(&"203.0.113.10:9444".to_string()));
        }
        other => panic!("expected peer list, got {other:?}"),
    }

    super::set_p2p_accept_inbound(&state, false, None)
        .await
        .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert!(!config.p2p_accept_inbound);
    assert!(!state.gossip.accepts_inbound().await);

    super::set_p2p_announce_addr(&state, " ".to_string())
        .await
        .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert!(config.p2p_announce_addr.is_none());
}

#[tokio::test]
async fn p2p_announce_setting_rejects_invalid_address() {
    let dir = tempfile::tempdir().unwrap();
    let state = auth_test_state(dir.path().join("config.json"), UiConfig::default()).await;

    let error = super::set_p2p_announce_addr(&state, "not-an-address".to_string())
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("invalid P2P announce address"));
}

#[tokio::test]
async fn setup_config_form_can_add_bootstrap_peer() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let state = auth_test_state(config_path.clone(), UiConfig::default()).await;

    super::apply_config_form(
        &state,
        super::ConfigForm {
            setup_complete: true,
            peer: " iuna.jhx.app:9444 ".to_string(),
        },
    )
    .await
    .unwrap();

    let config = config_store::load_or_create(&config_path).unwrap();
    assert!(config.setup_complete);
    assert_eq!(config.peers, vec!["iuna.jhx.app:9444"]);
    assert_eq!(
        state.peers.lock().await.addresses(),
        vec!["iuna.jhx.app:9444"]
    );
}

#[tokio::test]
async fn setup_config_form_requires_peer_for_placeholder_chain() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let state = auth_test_state(config_path.clone(), UiConfig::default()).await;

    let error = super::apply_config_form(
        &state,
        super::ConfigForm {
            setup_complete: true,
            peer: " ".to_string(),
        },
    )
    .await
    .unwrap_err();

    assert!(format!("{error:#}").contains("add a bootstrap peer"));
    let config = config_store::load_or_create(&config_path).unwrap();
    assert!(!config.setup_complete);
}

#[tokio::test]
async fn peer_management_rejects_empty_and_inbound_removal() {
    let dir = tempfile::tempdir().unwrap();
    let state = auth_test_state(dir.path().join("config.json"), UiConfig::default()).await;

    assert!(super::add_peer(&state, "   ".to_string()).await.is_err());
    state
        .peers
        .lock()
        .await
        .record_received("127.0.0.1:9555", 1);

    let result = super::remove_peer(&state, "127.0.0.1:9555".to_string()).await;
    assert!(result.is_err());
    assert_eq!(state.peers.lock().await.addresses(), Vec::<String>::new());
}

#[tokio::test]
async fn network_health_summarizes_sync_and_peer_errors() {
    let dir = tempfile::tempdir().unwrap();
    let state = auth_test_state(dir.path().join("config.json"), UiConfig::default()).await;
    let status = state.node.lock().await.status();
    let local = super::NetworkHealthLocalState {
        height: status.chain.height,
        pending_transactions: status.chain.pending_transactions,
    };
    let mempool = super::MempoolCounts {
        plain_transactions: 1,
        blinded_transactions: 2,
        blinded_reveals: 3,
    };

    let isolated = super::network_health(local, &[], mempool);
    assert!(!isolated.ok);
    assert_eq!(isolated.state, "isolated");
    assert_eq!(isolated.local_height, 0);
    assert_eq!(isolated.best_known_height, 0);
    assert_eq!(isolated.pending_plain_transactions, 1);
    assert_eq!(isolated.pending_blinded_transactions, 2);
    assert_eq!(isolated.pending_blinded_reveals, 3);

    let mut clock_peers = PeerBook::from_addresses(vec![
        "127.0.0.1:9450".to_string(),
        "127.0.0.1:9451".to_string(),
    ]);
    clock_peers.record_status("127.0.0.1:9450", 0, "tip".to_string());
    clock_peers.record_status("127.0.0.1:9451", 0, "tip".to_string());
    clock_peers.record_clock_observation("127.0.0.1:9450", PeerDirection::Outbound, 10_500, 10_000);
    clock_peers.record_clock_observation(
        "127.0.0.1:9451",
        PeerDirection::Outbound,
        11 * 60 * 1_000,
        10_000,
    );
    let clock_health = super::network_health_at(local, &clock_peers.list(), mempool, 10_000);
    assert_eq!(clock_health.network_time_offset_ms, Some(500));
    assert_eq!(clock_health.bad_clock_peers, 1);

    let syncing = super::network_health(
        local,
        &[PeerInfo {
            address: "127.0.0.1:9445".to_string(),
            direction: PeerDirection::Outbound,
            messages_sent: 1,
            messages_received: 1,
            last_known_height: Some(3),
            last_known_tip_hash: Some("remote-tip".to_string()),
            last_clock_offset_ms: None,
            last_clock_offset_accepted: None,
            last_clock_observed_ms: None,
            last_error: None,
            last_contact_ms: Some(10_000),
            last_success_ms: Some(10_000),
            last_error_ms: None,
            misbehavior_score: 0,
            banned_until_ms: None,
            ban_reason: None,
        }],
        mempool,
    );
    assert!(!syncing.ok);
    assert_eq!(syncing.state, "syncing");
    assert_eq!(syncing.best_known_height, 3);
    assert_eq!(syncing.lag_blocks, 3);

    let peer_errors = super::network_health(
        local,
        &[PeerInfo {
            address: "127.0.0.1:9446".to_string(),
            direction: PeerDirection::Outbound,
            messages_sent: 0,
            messages_received: 0,
            last_known_height: None,
            last_known_tip_hash: None,
            last_clock_offset_ms: None,
            last_clock_offset_accepted: None,
            last_clock_observed_ms: None,
            last_error: Some("connection refused".to_string()),
            last_contact_ms: Some(10_000),
            last_success_ms: None,
            last_error_ms: Some(10_000),
            misbehavior_score: 1,
            banned_until_ms: None,
            ban_reason: Some("connection refused".to_string()),
        }],
        mempool,
    );
    assert!(!peer_errors.ok);
    assert_eq!(peer_errors.state, "peer errors");
    assert_eq!(
        peer_errors.last_error.as_deref(),
        Some("127.0.0.1:9446: connection refused")
    );

    let stale = super::network_health_at(
        local,
        &[PeerInfo {
            address: "127.0.0.1:9447".to_string(),
            direction: PeerDirection::Outbound,
            messages_sent: 1,
            messages_received: 1,
            last_known_height: Some(0),
            last_known_tip_hash: Some("tip".to_string()),
            last_clock_offset_ms: None,
            last_clock_offset_accepted: None,
            last_clock_observed_ms: None,
            last_error: None,
            last_contact_ms: Some(1),
            last_success_ms: Some(1),
            last_error_ms: None,
            misbehavior_score: 0,
            banned_until_ms: None,
            ban_reason: None,
        }],
        mempool,
        PEER_STALE_AFTER_MS + 2,
    );
    assert!(!stale.ok);
    assert_eq!(stale.state, "stale");
    assert_eq!(stale.stale_peers, 1);

    let banned = super::network_health_at(
        local,
        &[PeerInfo {
            address: "127.0.0.1:9448".to_string(),
            direction: PeerDirection::Outbound,
            messages_sent: 0,
            messages_received: 0,
            last_known_height: None,
            last_known_tip_hash: None,
            last_clock_offset_ms: None,
            last_clock_offset_accepted: None,
            last_clock_observed_ms: None,
            last_error: Some("invalid block".to_string()),
            last_contact_ms: Some(10),
            last_success_ms: None,
            last_error_ms: Some(10),
            misbehavior_score: 3,
            banned_until_ms: Some(1_000),
            ban_reason: Some("invalid block".to_string()),
        }],
        mempool,
        20,
    );
    assert!(!banned.ok);
    assert_eq!(banned.state, "banned");
    assert_eq!(banned.banned_peers, 1);
}

#[test]
fn wallet_transactions_include_old_confirmed_transfers_without_burns_or_explorer_pagination() {
    let alice = Wallet::from_seed("wallet-history-alice");
    let bob = Wallet::from_seed("wallet-history-bob");
    let carol = Wallet::from_seed("wallet-history-carol");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 100);
    allocations.insert(bob.address().to_string(), 100);
    allocations.insert(carol.address().to_string(), 100);
    let ledger = crate::domain::Ledger::new(allocations.clone(), 1);
    let old_received = ledger.build_transfer(&bob, alice.address(), 31, 0).unwrap();
    let pending_burn = ledger.build_burn(&alice, 2, 1).unwrap();
    let carol_transfer = ledger.build_transfer(&carol, bob.address(), 5, 0).unwrap();
    let carol_burn = ledger.build_burn(&carol, 1, 0).unwrap();
    let chain = vec![
        fake_block(30, vec![carol_transfer]),
        fake_block(31, vec![old_received.clone()]),
        fake_block(32, vec![carol_burn]),
    ];

    let snapshot = fake_snapshot(allocations, chain.clone());
    let outputs = super::known_output_index(&snapshot, std::slice::from_ref(&pending_burn));
    let rows = wallet_transaction_rows(
        alice.address(),
        vec![pending_burn.clone()],
        Vec::new(),
        &chain,
        &BTreeMap::new(),
        &outputs,
        WalletTransactionFilters::default(),
    );

    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].signature, pending_burn.signature());
    assert_eq!(rows[0].signature, old_received.signature());
    assert_eq!(rows[0].inputs[0].amount, Some(100));
    assert_eq!(rows[0].status, "confirmed");
    assert_eq!(rows[0].block_height, Some(31));
    assert_eq!(rows[0].timestamp_ms, Some(31));
    assert_eq!(rows[0].block_finalizer.as_deref(), Some("miner"));
    assert_eq!(rows[0].direction, "received");
}

#[test]
fn wallet_transactions_show_owned_blinded_payloads_as_pending_blind() {
    let alice = Wallet::from_seed("wallet-blind-pending-alice");
    let bob = Wallet::from_seed("wallet-blind-pending-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 100);
    allocations.insert(bob.address().to_string(), 100);
    let ledger = Ledger::new(allocations.clone(), 1);
    let pending_blind = ledger.build_transfer(&alice, bob.address(), 12, 3).unwrap();
    let snapshot = fake_snapshot(allocations, Vec::new());
    let outputs = super::known_output_index(&snapshot, &[]);

    let rows = wallet_transaction_rows(
        alice.address(),
        Vec::new(),
        vec![pending_blind.clone()],
        &[],
        &BTreeMap::new(),
        &outputs,
        WalletTransactionFilters::default(),
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "transfer");
    assert_eq!(rows[0].status, "pending");
    assert_eq!(rows[0].timestamp_ms, None);
    assert!(rows[0].blinded);
    assert_eq!(rows[0].direction, "sent");
    assert_eq!(rows[0].to.as_deref(), Some(bob.address()));
    assert_eq!(rows[0].amount, 12);
    assert_eq!(rows[0].fee, 3);
    assert_eq!(rows[0].signature, pending_blind.signature());
}

#[test]
fn wallet_transaction_query_defaults_to_tx_only() {
    assert_eq!(
        WalletTransactionFilters::from_query(WalletTransactionsQuery::default()),
        WalletTransactionFilters::default()
    );
    assert_eq!(
        WalletTransactionFilters::from_query(WalletTransactionsQuery {
            tx: Some(false),
            mine: Some(true),
            burn: Some(true),
            offset: None,
            limit: None,
        }),
        WalletTransactionFilters {
            transfer: false,
            mine: true,
            burn: true,
        }
    );
}

#[test]
fn page_items_returns_bounded_slices_with_next_offset() {
    let page = super::page_items(
        vec![1, 2, 3, 4, 5],
        super::PageQuery {
            offset: Some(1),
            limit: Some(2),
        },
    );

    assert_eq!(page.items, vec![2, 3]);
    assert_eq!(page.offset, 1);
    assert_eq!(page.limit, 2);
    assert_eq!(page.total, 5);
    assert!(page.has_more);
    assert_eq!(page.next_offset, Some(3));
}

#[test]
fn page_items_clamps_limit_and_empty_tail() {
    let page = super::page_items(
        vec![1, 2],
        super::PageQuery {
            offset: Some(20),
            limit: Some(0),
        },
    );

    assert!(page.items.is_empty());
    assert_eq!(page.offset, 2);
    assert_eq!(page.limit, 1);
    assert_eq!(page.total, 2);
    assert!(!page.has_more);
    assert_eq!(page.next_offset, None);
}

#[test]
fn mine_transaction_views_include_protocol_finalizer_fee() {
    let alice = Wallet::from_seed("wallet-mine-fee-alice");
    let ledger = Ledger::new(BTreeMap::new(), 1);
    let mine = ledger.build_mine(alice.address()).unwrap();
    let chain = vec![fake_block(1, vec![mine.clone()])];
    let snapshot = fake_snapshot(BTreeMap::new(), chain.clone());
    let outputs = super::known_output_index(&snapshot, &[]);

    let rows = wallet_transaction_rows(
        alice.address(),
        Vec::new(),
        Vec::new(),
        &chain,
        &BTreeMap::new(),
        &outputs,
        WalletTransactionFilters {
            transfer: false,
            mine: true,
            burn: false,
        },
    );
    let transaction = super::ui_transaction(&mine, &outputs);

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].amount, mine.amount());
    assert_eq!(rows[0].fee, MINE_FINALIZER_FEE);
    assert_eq!(transaction.amount, mine.amount());
    assert_eq!(transaction.fee, MINE_FINALIZER_FEE);
}

#[test]
fn wallet_transactions_include_public_mine_actions() {
    let alice = Wallet::from_seed("wallet-revealed-mine-alice");
    let ledger = Ledger::new(
        BTreeMap::from([(alice.address().to_string(), 2 * MICRO_IUNA)]),
        1,
    );
    let mine = ledger.build_mine(alice.address()).unwrap();
    let mut mine_block = fake_block(8, vec![mine.clone()]);
    mine_block.reward = mine.fee();
    let chain = vec![mine_block.clone()];
    let snapshot = fake_snapshot(BTreeMap::new(), chain.clone());
    let revealed_by_height = super::revealed_transactions_by_height(&snapshot);
    let outputs = super::known_output_index(&snapshot, &[]);

    let rows = wallet_transaction_rows(
        alice.address(),
        Vec::new(),
        Vec::new(),
        &chain,
        &revealed_by_height,
        &outputs,
        WalletTransactionFilters {
            transfer: false,
            mine: true,
            burn: false,
        },
    );

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].kind, "mine");
    assert_eq!(rows[0].status, "confirmed");
    assert_eq!(rows[0].block_height, Some(8));
    assert_eq!(rows[0].timestamp_ms, Some(8));
    assert_eq!(rows[0].block_finalizer.as_deref(), Some("miner"));
    assert_eq!(rows[0].direction, "received");
    assert_eq!(rows[0].amount, mine.amount());
    assert_eq!(rows[0].fee, MINE_FINALIZER_FEE);
    assert!(!rows[0].blinded);
    assert_eq!(rows[0].signature, mine.signature());
}

#[test]
fn burn_wallet_transactions_require_burn_filter() {
    let alice = Wallet::from_seed("wallet-burn-filter-alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10);
    let ledger = Ledger::new(allocations.clone(), 1);
    let burn = ledger.build_burn(&alice, 3, 1).unwrap();
    let snapshot = fake_snapshot(allocations, Vec::new());
    let outputs = super::known_output_index(&snapshot, std::slice::from_ref(&burn));

    let default_rows = wallet_transaction_rows(
        alice.address(),
        vec![burn.clone()],
        Vec::new(),
        &[],
        &BTreeMap::new(),
        &outputs,
        WalletTransactionFilters::default(),
    );
    let burn_rows = wallet_transaction_rows(
        alice.address(),
        vec![burn.clone()],
        Vec::new(),
        &[],
        &BTreeMap::new(),
        &outputs,
        WalletTransactionFilters {
            transfer: false,
            mine: false,
            burn: true,
        },
    );

    assert!(default_rows.is_empty());
    assert_eq!(burn_rows.len(), 1);
    assert_eq!(burn_rows[0].kind, "burn");
    assert_eq!(burn_rows[0].direction, "burned");
    assert_eq!(burn_rows[0].amount, 3);
    assert_eq!(burn_rows[0].fee, 1);
}

#[test]
fn wallet_utxo_rows_include_pending_spent_outputs_as_disabled() {
    let alice = Wallet::from_seed("wallet-utxo-pending-alice");
    let bob = Wallet::from_seed("wallet-utxo-pending-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10);
    let mut ledger = Ledger::new(allocations, 1);
    let pending = ledger.build_transfer(&alice, bob.address(), 3, 0).unwrap();
    let Transaction::Transfer { inputs, .. } = &pending else {
        panic!("expected transfer");
    };
    let spent_outpoint = inputs[0].outpoint.clone();

    ledger.submit_transaction(pending).unwrap();
    let rows = wallet_utxo_rows(&ledger, alice.address());

    assert!(rows.iter().any(|row| row.outpoint == spent_outpoint));
    assert!(
        rows.iter()
            .any(|row| row.outpoint == spent_outpoint && !row.spendable)
    );
}

#[test]
fn selectable_wallet_utxo_rows_include_only_spendable_outputs() {
    let alice = Wallet::from_seed("wallet-utxo-selectable-alice");
    let bob = Wallet::from_seed("wallet-utxo-selectable-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10);
    let mut ledger = Ledger::new(allocations, 1);
    let pending = ledger.build_transfer(&alice, bob.address(), 3, 0).unwrap();
    let Transaction::Transfer { inputs, .. } = &pending else {
        panic!("expected transfer");
    };
    let spent_outpoint = inputs[0].outpoint.clone();

    ledger.submit_transaction(pending).unwrap();
    let rows = super::selectable_wallet_utxo_rows(&ledger, alice.address());

    assert!(!rows.iter().any(|row| row.outpoint == spent_outpoint));
    assert!(rows.iter().all(|row| row.spendable));
}

#[test]
fn wallet_utxo_rows_treat_owned_blinded_spends_as_pending() {
    let alice = Wallet::from_seed("wallet-utxo-blind-pending-alice");
    let bob = Wallet::from_seed("wallet-utxo-blind-pending-bob");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10);
    let ledger = Ledger::new(allocations, 1);
    let mut node = NodeCore::from_ledger(alice.clone(), ledger, 0);
    let pending = node.transfer_with_fee(bob.address(), 3, 0).unwrap();
    let Transaction::Transfer { inputs, .. } = &pending else {
        panic!("expected transfer");
    };
    let spent_outpoint = inputs[0].outpoint.clone();
    let wallet_view = node.wallet_view_ledger().unwrap();

    let rows = wallet_utxo_rows(&wallet_view, alice.address());
    let selectable = super::selectable_wallet_utxo_rows(&wallet_view, alice.address());

    assert!(
        rows.iter()
            .any(|row| row.outpoint == spent_outpoint && !row.spendable)
    );
    assert!(!selectable.iter().any(|row| row.outpoint == spent_outpoint));
}

#[test]
fn wallet_utxo_rows_keep_local_anchor_spends_visible_as_pending() {
    let alice = Wallet::from_seed("wallet-utxo-local-anchor-alice");
    let mut allocations = BTreeMap::new();
    allocations.insert(alice.address().to_string(), 10 * MICRO_IUNA);
    let ledger = Ledger::new_with_genesis_burns(
        allocations,
        vec![GenesisBurn::new(alice.address(), MICRO_IUNA)],
        1,
    )
    .unwrap();
    let mut node =
        NodeCore::from_ledger_with_burn_fee_and_enabled(alice.clone(), ledger, true, 1, 0);

    let plan = node.prepare_automatic_finalization(1);
    assert!(plan.burned.is_some());
    let rows = wallet_utxo_rows(&node.wallet_view_ledger().unwrap(), alice.address());

    assert!(!rows.is_empty());
    assert!(rows.iter().any(|row| !row.spendable));
}

fn fake_block(height: u64, transactions: Vec<Transaction>) -> Block {
    Block {
        height,
        prev_hash: format!("prev-{height}"),
        timestamp_ms: height,
        miner: "miner".to_string(),
        finalizer_mode: crate::domain::FinalizerMode::Ticket,
        finalizer_rank: 0,
        reward: 100,
        vdf_rounds: 0,
        vdf_output: "vdf".to_string(),
        leader_proof: None,
        blinded_transactions: Vec::new(),
        reveal_bundle_section: RevealBundleSection::default(),
        transactions,
        hash: format!("hash-{height}"),
    }
}

fn fake_snapshot(
    genesis_allocations: BTreeMap<String, Amount>,
    blocks: Vec<Block>,
) -> ChainSnapshot {
    ChainSnapshot {
        genesis_allocations,
        vdf_rounds: 1,
        launch_profile: LaunchProfile::default(),
        blocks,
    }
}

fn metric_row(
    height: u64,
    block_time_ms: Option<u64>,
    vdf_rounds: u64,
) -> crate::adapters::chain_store::BlockMetricRow {
    crate::adapters::chain_store::BlockMetricRow {
        height,
        block_hash: format!("hash-{height}"),
        timestamp_ms: height,
        block_time_ms,
        mine_difficulty_bits: 12,
        circulating_supply: 100,
        known_wallet_addresses: 1,
        transaction_count: 0,
        transfer_count: 0,
        burn_count: 0,
        mine_count: 0,
        burned_amount: 0,
        total_burned_amount: 0,
        fees_amount: 0,
        reward_amount: 0,
        vdf_rounds,
        finalizer_rank: 0,
    }
}

#[test]
fn metrics_response_skips_bootstrap_points_for_block_time_and_vdf_rounds() {
    let response = super::metrics_response(
        true,
        vec![
            metric_row(0, None, 0),
            metric_row(1, Some(1_764_000_000_000), 0),
            metric_row(2, Some(600_000), 120),
            metric_row(3, Some(610_000), 130),
        ],
    );

    let block_time = response
        .charts
        .iter()
        .find(|chart| chart.id == "block-time")
        .expect("block time chart should exist");
    assert_eq!(
        block_time
            .points
            .iter()
            .map(|point| (point.height, point.value))
            .collect::<Vec<_>>(),
        vec![(2, 600.0), (3, 610.0)]
    );

    let vdf_rounds = response
        .charts
        .iter()
        .find(|chart| chart.id == "vdf-rounds")
        .expect("VDF rounds chart should exist");
    assert_eq!(
        vdf_rounds
            .points
            .iter()
            .map(|point| (point.height, point.value))
            .collect::<Vec<_>>(),
        vec![(2, 120.0), (3, 130.0)]
    );

    let known_wallet_addresses = response
        .charts
        .iter()
        .find(|chart| chart.id == "known-wallet-addresses")
        .expect("known wallet addresses chart should exist");
    assert_eq!(
        known_wallet_addresses
            .points
            .iter()
            .map(|point| (point.height, point.value))
            .collect::<Vec<_>>(),
        vec![(0, 1.0), (1, 1.0), (2, 1.0), (3, 1.0)]
    );
}

#[test]
fn metrics_screen_includes_block_range_filter() {
    assert!(super::INDEX_HTML.contains("iuna-ui.js?v=100"));
    assert!(super::INDEX_HTML.contains("aria-label=\"Metrics block range\""));
    assert!(super::INDEX_HTML.contains("setMetricsRange(100)"));
    assert!(super::INDEX_HTML.contains("setMetricsRange(1000)"));
    assert!(super::INDEX_HTML.contains("setMetricsRange('all')"));
    assert!(super::INDEX_HTML.contains("Known addresses"));
    assert!(super::INDEX_HTML.contains("knownWalletAddresses"));
}

#[test]
fn reveal_mempool_items_show_unknown_fee_label() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(app_js.contains("txFeeLabel(tx)"));
    assert!(app_js.contains("!tx?.revealed && tx?.kind === \"reveal\""));
    assert!(app_js.contains("unknown until reveal"));
    assert!(
        app_js.contains("!tx?.revealed && (tx?.kind === \"blinded\" || tx?.kind === \"reveal\")")
    );
    assert!(app_js.contains("mempoolFirstSeenHeights"));
    assert!(app_js.contains("mempoolFirstSeenAt"));
    assert!(
        app_js.contains("this.trackMempoolFirstSeenHeights({ append: options.replace !== true })")
    );
    assert!(app_js.contains("return rightSeenAt - leftSeenAt"));
    assert!(app_js.contains("syncMempoolBlockMarker"));
    assert!(app_js.contains("this.status.chain?.height ?? this.lastBlockMempoolHeight"));
    assert!(app_js.contains("mempoolItemClass"));
    assert!(app_js.contains("walletTxTimeLabel(tx)"));
    assert!(app_js.contains("timestampMs ?? tx?.timestamp_ms"));
    assert!(super::INDEX_HTML.contains("x-text=\"txFeeLabel(tx)\""));
    assert!(super::INDEX_HTML.contains("x-text=\"txFeeLabel(selectedTransaction?.tx)\""));
    assert!(super::INDEX_HTML.contains("<span class=\"tx-label\">Time</span>"));
    assert!(super::INDEX_HTML.contains("x-text=\"walletTxTimeLabel(tx)\""));
}

#[test]
fn wallet_screen_includes_address_book_alias_controls() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(super::INDEX_HTML.contains("<h3>Address Book</h3>"));
    assert!(super::INDEX_HTML.contains("saveAddressBookEntry"));
    assert!(super::INDEX_HTML.contains("addressBookEntries()"));
    assert!(super::INDEX_HTML.contains("openAddressBookModal()"));
    assert!(super::INDEX_HTML.contains("addressBookModalOpen"));
    assert!(super::INDEX_HTML.contains("openAddressBookPicker()"));
    assert!(super::INDEX_HTML.contains("addressBookPickerOpen"));
    assert!(super::INDEX_HTML.contains("selectTransferContact(entry.address)"));
    assert!(super::INDEX_HTML.contains("editAddressBookEntry(entry)"));
    assert!(
        super::INDEX_HTML.contains("removeAddressBookEntry({ address: addressBookEditingAddress")
    );
    assert!(super::INDEX_HTML.contains("aria-label=\"Choose contact\""));
    assert!(super::INDEX_HTML.contains("aria-label=\"Delete contact\""));
    assert!(super::INDEX_HTML.contains("shortAddressLabel(tx.from)"));
    assert!(super::INDEX_HTML.contains("addressLabel(input.owner)"));
    assert!(app_js.contains("addressBook: {}"));
    assert!(app_js.contains("addressBookVersion: 0"));
    assert!(app_js.contains("addressBookPickerOpen: false"));
    assert!(app_js.contains("this.config.address_book || this.config.addressBook"));
    assert!(app_js.contains("options.addressBookVersion >= this.addressBookVersion"));
    assert!(app_js.contains("async saveAddressBookEntry()"));
    assert!(app_js.contains("Address is already saved"));
    assert!(app_js.contains("validAddressBookAddress(address)"));
    assert!(app_js.contains("openAddressBookPicker()"));
    assert!(app_js.contains("await this.submitForm(\"/api/address-book\""));
    assert!(app_js.contains("\"/api/address-book\""));
    assert!(app_js.contains("addressLabel(address)"));
    assert!(app_js.contains("shortAddressLabel(address)"));
}

#[test]
fn initial_setup_includes_node_mode_choices() {
    assert!(super::INDEX_HTML.contains("aria-label=\"Initial node mode\""));
    assert!(super::INDEX_HTML.contains("selectSetupNodeMode('wallet')"));
    assert!(super::INDEX_HTML.contains("selectSetupNodeMode('non-listening')"));
    assert!(super::INDEX_HTML.contains("selectSetupNodeMode('listening')"));
    assert!(super::INDEX_HTML.contains("setupNodeMode === 'listening'"));
    assert!(super::INDEX_HTML.contains("x-model.number=\"p2pBindPort\""));
    assert!(super::INDEX_HTML.contains("Change later in Settings"));
}

#[test]
fn setup_completion_refreshes_chain_data() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(
        app_js.contains("await this.refresh({ force: true });\n        this.setupFeedback = null;")
    );
    assert!(
        !app_js.contains(
            "await this.refreshConfig();\n        await this.resetPagedDataset(\"peer\");"
        )
    );
}

#[test]
fn setup_completion_applies_selected_node_mode() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(app_js.contains("setupNodeMode: \"wallet\""));
    assert!(app_js.contains("async applySetupNodeMode()"));
    assert!(app_js.contains("await this.applySetupNodeMode();"));
    assert!(app_js.contains("\"/api/settings/p2p-inbound\""));
    assert!(app_js.contains("bind_port: this.p2pBindPortValue()"));
    assert!(app_js.contains("this.setUiMode(mode === \"wallet\" ? \"basic\" : \"advanced\")"));
}

#[test]
fn p2p_bind_port_changes_show_global_restart_notice() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(super::INDEX_HTML.contains("Bind port"));
    assert!(super::INDEX_HTML.contains("persistent-banner"));
    assert!(super::INDEX_HTML.contains("p2pRestartRequired()"));
    assert!(app_js.contains("p2pBindPort: 9444"));
    assert!(app_js.contains("p2pConfiguredBindAddr()"));
    assert!(app_js.contains("p2pRestartMessage()"));
    assert!(app_js.contains("Restart iuna to close the public P2P listener."));
    assert!(app_js.contains("0.0.0.0:${port}"));
}

#[test]
fn settings_includes_dangerous_chain_reset_flow() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(super::INDEX_HTML.contains("Danger Zone"));
    assert!(super::INDEX_HTML.contains("Delete local chain"));
    assert!(super::INDEX_HTML.contains("Type <strong>RESET</strong> to confirm."));
    assert!(super::INDEX_HTML.contains("class=\"danger\""));
    assert!(super::INDEX_HTML.contains("chainResetModalOpen"));
    assert!(app_js.contains("chainResetModalOpen: false"));
    assert!(app_js.contains("async resetLocalChain()"));
    assert!(app_js.contains("\"/api/settings/chain-reset\""));
    assert!(app_js.contains("confirm: this.chainResetConfirm"));
    assert!(app_js.contains(
        "this.showFlash(\"Local chain deleted. Sync requested from peers.\", \"success\")"
    ));
    assert!(!app_js.contains("this.showSettingsFeedback(\"Local chain deleted."));
    assert!(!app_js.contains("this.showSettingsFeedback(error.message, \"error\");\n      } finally {\n        this.chainResetBusy = false;"));
}

#[tokio::test]
async fn chain_reset_deletes_local_chain_and_returns_to_placeholder() {
    let dir = tempfile::tempdir().unwrap();
    let state = auth_test_state(dir.path().join("config.json"), UiConfig::default()).await;
    let wallet = Wallet::from_seed("http-chain-reset-alice");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 10);
    let ledger =
        Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
            .unwrap();
    state
        .chain_store
        .save_with_metrics(&ledger.snapshot(), true)
        .unwrap();
    {
        let mut node = state.node.lock().await;
        *node = NodeCore::from_ledger(wallet.clone(), ledger, 1);
    }
    {
        let mut cache = state.ui_cache.lock().await;
        cache.tip_hash = Some("stale-tip".to_string());
    }

    let error = super::reset_local_chain(&state, "nope").await.unwrap_err();
    assert!(error.to_string().contains("type RESET"));
    assert!(state.chain_store.load().unwrap().is_some());
    assert!(state.node.lock().await.has_real_chain());

    super::reset_local_chain(&state, "RESET").await.unwrap();

    let node = state.node.lock().await;
    assert!(node.ledger().is_setup_placeholder());
    assert_eq!(node.status().wallet_address, wallet.address());
    drop(node);
    assert!(state.chain_store.load().unwrap().is_none());
    assert!(state.chain_store.load_metrics().unwrap().is_empty());
    assert!(state.ui_cache.lock().await.tip_hash.is_none());
}

#[test]
fn polling_refreshes_paged_datasets_without_visible_loaders() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(app_js.contains("setInterval(() => this.refresh({ silent: true }), 5000)"));
    assert!(
            app_js.contains(
                "return this.authLoaded && this.auth.configured === true && this.auth.authenticated === true;"
            )
        );
    assert!(app_js.contains(
        "async refreshNow(options = {}) {\n      if (!this.canUseProtectedApi()) return;"
    ));
    assert!(app_js.contains("refreshPromise: null"));
    assert!(app_js.contains("if (this.refreshPromise)"));
    assert!(app_js.contains("return this.refreshPromise;"));
    assert!(app_js.contains("options.force === true"));
    assert!(app_js.contains("this.setTab(this.tabFromHash());"));
    assert!(app_js.contains("const shouldLoadBlocks = tab === \"chain\" || tab === \"mining\";"));
    assert!(app_js.contains("const shouldLoadP2pMetrics = tab === \"p2p\";"));
    assert!(app_js.contains("const shouldLoadMetrics = tab === \"metrics\";"));
    assert!(
        app_js
            .contains("if (tab === \"wallet\") pagedDatasets.push(\"walletTx\", \"walletUtxo\");")
    );
    assert!(app_js.contains("if (tab === \"chain\") pagedDatasets.push(\"mempool\");"));
    assert!(app_js.contains("if (tab === \"p2p\") pagedDatasets.push(\"peer\");"));
    assert!(app_js.contains("cache: \"no-store\""));
    assert!(app_js.contains("async fetchWithTimeout(path, options = {})"));
    assert!(app_js.contains("controller.abort()"));
    assert!(app_js.contains("async refreshPagedDataset(kind, options = {}) {\n      if (!this.canUseProtectedApi()) return;"));
    assert!(
        app_js
            .contains("async loadNextPage(kind) {\n      if (!this.canUseProtectedApi()) return;")
    );
    assert!(
        app_js.contains("async loadOlderBlocks() {\n      if (!this.canUseProtectedApi()) return;")
    );
    assert!(app_js.contains("this.stopPolling();\n          await this.refreshAuth();"));
    assert!(app_js.contains("backgroundLoading"));
    assert!(app_js.contains("options.silent === true ? \"backgroundLoading\" : \"loading\""));
}

#[test]
fn wallet_pending_blinded_transactions_are_labeled() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(app_js.contains("Pending blind"));
    assert!(app_js.contains("tx.blinded"));
}

#[test]
fn mine_screen_shows_fixed_pow_reward_without_burn_slider() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(app_js.contains("powMineReward()"));
    assert!(super::INDEX_HTML.contains("Search for PoW actions that mint a fixed IUNA reward."));
    assert!(super::INDEX_HTML.contains("amountLabel(powMineReward())"));
    assert!(super::INDEX_HTML.contains("aria-label=\"Local mining status\""));
    assert!(super::INDEX_HTML.contains("PoB State"));
    assert!(super::INDEX_HTML.contains("PoW State"));
    assert!(super::INDEX_HTML.contains("Workers"));
    assert!(super::INDEX_HTML.contains("Selected Finalizer"));
    assert!(app_js.contains("pobStatusLabel()"));
    assert!(app_js.contains("powStatusShortLabel()"));
    assert!(app_js.contains("setPowMiningWorkers(workers)"));
    assert!(app_js.contains("localMiningMempoolLabel()"));
    assert!(app_js.contains("miningEventLog()"));
    assert!(app_js.contains("miningEventLimit: 1000"));
    assert!(app_js.contains("slice(0, this.miningEventLimit)"));
    assert!(super::INDEX_HTML.contains("aria-label=\"Mining event log\""));
    assert!(super::INDEX_HTML.contains("miningEventLog().length === 0"));
    assert!(super::INDEX_HTML.contains("mining-event-empty"));
    assert!(app_js.contains("Resource budget:"));
    assert!(app_js.contains("isPowMineSuccessStatus"));
    assert!(app_js.contains("You mined a PoW action"));
    assert!(app_js.contains("Waiting for a finalizer to include it in a block."));
    assert!(app_js.contains("Observed block"));
    assert!(app_js.contains("Finalized by"));
    assert!(app_js.contains("You finalized block"));
    assert!(app_js.contains("if (!locallyFinalized)"));
    assert!(app_js.contains("last?.title === title"));
    assert!(app_js.contains("this.miningEventState.pob = enabled ? \"on\" : \"off\";"));
    assert!(app_js.contains("Automatic burn prepared at height"));
    assert!(app_js.contains("Eligible for the next block opportunity."));
    assert!(app_js.contains("!Number.isFinite(timestampMs)"));
    assert!(!super::INDEX_HTML.contains("Needs burns"));
}

#[test]
fn block_detail_finalizer_opens_burn_leader_ranks_modal() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(super::INDEX_HTML.contains("openBurnLeaderRanksModal(selectedBlock)"));
    assert!(super::INDEX_HTML.contains("id=\"burn-ranks-title\""));
    assert!(super::INDEX_HTML.contains("burnLeaderRanks(selectedBurnLeaderBlock)"));
    assert!(app_js.contains("selectedBurnLeaderBlock"));
    assert!(app_js.contains("burnLeaderRankLabel(rank)"));
    assert!(app_js.contains("block?.burn_leader_ranks"));
    assert!(super::INDEX_HTML.contains("rank.ticket_id ?? rank.ticketId"));
}

#[test]
fn block_loading_skeleton_matches_block_card_layout() {
    let app_js = include_str!("../../../www/assets/iuna-ui.js");
    assert!(super::INDEX_HTML.contains("block-skeleton-group"));
    assert!(super::INDEX_HTML.contains("block-card block-card-skeleton skeleton-card"));
    assert!(super::INDEX_HTML.contains("skeleton-block-height"));
    assert!(super::INDEX_HTML.contains("skeleton-block-meta"));
    assert!(super::INDEX_HTML.contains("skeleton-block-miner"));
    assert!(app_js.contains("const hadBlocks = this.blocks.length > 0;"));
    assert!(app_js.contains("const previousScrollWidth = hadBlocks ? rail?.scrollWidth ?? 0 : 0;"));
    assert!(app_js.contains("&& hadBlocks"));
    assert!(app_js.contains("this.$nextTick(() => this.resetBlockRailPosition());"));
    assert!(app_js.contains("resetBlockRailPosition()"));
}

#[tokio::test]
async fn startup_prewarm_populates_chain_view_cache_before_first_request() {
    let dir = tempfile::tempdir().unwrap();
    let state = auth_test_state(dir.path().join("config.json"), UiConfig::default()).await;
    let expected_tip = {
        let node = state.node.lock().await;
        node.chain_snapshot()
            .blocks
            .last()
            .map(|block| block.hash.clone())
    };
    let sentinel = OutPoint {
        txid: "persisted-ui-index-sentinel".to_string(),
        index: 7,
    };
    let connection = rusqlite::Connection::open(state.chain_store.path()).unwrap();
    connection
        .execute(
            r#"
INSERT INTO ui_cache_meta (id, schema_version, tip_hash, updated_at_ms)
VALUES (1, 1, ?1, 0)
"#,
            rusqlite::params![expected_tip.as_ref().unwrap()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO ui_burn_leader_rank_blocks (block_hash) VALUES (?1)",
            rusqlite::params![expected_tip.as_ref().unwrap()],
        )
        .unwrap();
    connection
        .execute(
            r#"
INSERT INTO ui_output_index (txid, output_index, address, amount)
VALUES (?1, ?2, ?3, ?4)
"#,
            rusqlite::params![&sentinel.txid, sentinel.index, "cached-address", 123_u64],
        )
        .unwrap();
    assert!(state.ui_cache.lock().await.tip_hash.is_none());

    super::prewarm_chain_view_cache(state.clone())
        .await
        .unwrap();

    let cache = state.ui_cache.lock().await;
    assert_eq!(cache.tip_hash, expected_tip);
    assert!(
        cache
            .tip_hash
            .as_ref()
            .is_some_and(|tip_hash| cache.burn_leader_ranks_by_hash.contains_key(tip_hash))
    );
    assert_eq!(
        cache.outputs.get(&sentinel),
        Some(&TxOutput {
            address: "cached-address".to_string(),
            amount: 123,
        })
    );
}

async fn auth_test_state(config_path: std::path::PathBuf, config: UiConfig) -> HttpState {
    config_store::save(&config_path, &config).unwrap();
    let wallet_path = config_path.with_file_name("wallet.json");
    let (wallet, _) = wallet_store::replace_with_generated_seed_phrase(&wallet_path).unwrap();
    let ledger = Ledger::new(BTreeMap::new(), 1);
    let node = Arc::new(Mutex::new(NodeCore::from_ledger(wallet, ledger, 0)));
    let peers = Arc::new(Mutex::new(PeerBook::default()));
    let gossip = GossipNetwork::new_for_tests(node.clone(), peers.clone());
    let chain_store = SqliteChainStore::open(config_path.with_file_name("chain.sqlite3"))
        .expect("test chain store should open");
    HttpState {
        node,
        peers,
        gossip,
        ui_config: Arc::new(Mutex::new(
            config_store::load_or_create(&config_path).unwrap(),
        )),
        config_path,
        chain_store,
        wallet_path,
        stratum: StratumStatus {
            enabled: false,
            listen_addr: None,
        },
        auth_sessions: Arc::new(Mutex::new(BTreeMap::new())),
        auth_backoff: Arc::new(Mutex::new(BTreeMap::new())),
        ui_cache: Arc::new(Mutex::new(super::UiChainCache::default())),
    }
}

fn auth_test_app(state: HttpState) -> Router {
    Router::new()
        .route("/api/auth/status", get(api_auth_status))
        .route("/api/auth/setup", post(api_auth_setup_form))
        .route("/api/auth/login", post(api_auth_login_form))
        .route(
            "/api/auth/change-password",
            post(api_auth_change_password_form),
        )
        .route("/favicon.ico", get(super::favicon))
        .route("/api/protected", get(protected_auth_test_endpoint))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            require_auth_middleware,
        ))
        .with_state(state)
}

async fn protected_auth_test_endpoint() -> &'static str {
    "protected"
}

struct TestHttpResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

async fn http_request(
    app: Router,
    method: Method,
    path: &str,
    cookie: Option<&str>,
    body: &str,
) -> TestHttpResponse {
    let mut builder = Request::builder()
        .method(method.clone())
        .uri(path)
        .header(header::ACCEPT, "application/json")
        .header(header::HOST, "127.0.0.1:18661")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    if matches!(method, Method::POST | Method::DELETE) {
        builder = builder.header(header::ORIGIN, "http://127.0.0.1:18661");
    }
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    let response = app
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = String::from_utf8(
        to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap();
    TestHttpResponse {
        status,
        headers,
        body,
    }
}

fn set_cookie_pair(headers: &HeaderMap) -> String {
    let header = headers
        .get(header::SET_COOKIE)
        .and_then(|value| value.to_str().ok())
        .expect("response should include Set-Cookie header");
    header.split(';').next().unwrap().to_string()
}

#[tokio::test]
async fn burn_settings_config_persistence_updates_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let ui_config = Arc::new(Mutex::new(UiConfig {
        setup_complete: true,
        ..UiConfig::default()
    }));
    let initial_config = ui_config.lock().await.clone();
    config_store::save(&config_path, &initial_config).expect("initial config should save");

    persist_burn_settings_config(
        &ui_config,
        &config_path,
        true,
        50 * MICRO_IUNA,
        3 * MICRO_IUNA,
    )
    .await
    .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();

    assert!(config.mining_enabled);
    assert_eq!(config.burn_per_block, 50 * MICRO_IUNA);
    assert_eq!(config.burn_fee, 3 * MICRO_IUNA);
}

#[tokio::test]
async fn enabled_burn_settings_reject_zero_amount() {
    let dir = tempfile::tempdir().unwrap();
    let state = auth_test_state(
        dir.path().join("config.json"),
        UiConfig {
            setup_complete: true,
            ..UiConfig::default()
        },
    )
    .await;

    let error = super::set_burn_settings(&state, true, 0, MICRO_IUNA)
        .await
        .unwrap_err();

    assert!(format!("{error:#}").contains("greater than zero"));
}

#[tokio::test]
async fn metrics_setting_persists_config_and_clears_rows_when_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let state = auth_test_state(
        config_path.clone(),
        UiConfig {
            setup_complete: true,
            ..UiConfig::default()
        },
    )
    .await;
    let wallet = Wallet::from_seed("metrics-setting-wallet");
    let mut genesis = BTreeMap::new();
    genesis.insert(wallet.address().to_string(), 10);
    let ledger = Ledger::new_with_genesis_burns(
        genesis,
        vec![crate::domain::GenesisBurn::new(wallet.address(), 1)],
        1,
    )
    .unwrap();
    *state.node.lock().await = NodeCore::from_ledger(wallet, ledger, 0);

    super::set_keep_track_of_metrics(&state, true)
        .await
        .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert!(config.keep_track_of_metrics);
    assert!(!state.chain_store.load_metrics().unwrap().is_empty());

    super::set_keep_track_of_metrics(&state, false)
        .await
        .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();
    assert!(!config.keep_track_of_metrics);
    assert!(state.chain_store.load_metrics().unwrap().is_empty());
}

#[tokio::test]
async fn pow_mining_config_persistence_updates_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.json");
    let ui_config = Arc::new(Mutex::new(UiConfig {
        setup_complete: true,
        ..UiConfig::default()
    }));
    let initial_config = ui_config.lock().await.clone();
    config_store::save(&config_path, &initial_config).expect("initial config should save");

    persist_pow_mining_config(&ui_config, &config_path, true, 4)
        .await
        .unwrap();
    let config = config_store::load_or_create(&config_path).unwrap();

    assert!(config.pow_mining_enabled);
    assert_eq!(config.pow_mining_workers, 4);
}

#[test]
fn transfer_form_requires_recipient_amount_and_fee() {
    let error = validate_transfer_form(TransferForm {
        to: " ".to_string(),
        amount: 1,
        fee_per_byte: Some(1),
        utxos: String::new(),
    })
    .unwrap_err();
    assert!(error.to_string().contains("recipient is required"));

    let error = validate_transfer_form(TransferForm {
        to: "abc".to_string(),
        amount: 0,
        fee_per_byte: Some(1),
        utxos: String::new(),
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("amount must be greater than zero")
    );

    let error = validate_transfer_form(TransferForm {
        to: "abc".to_string(),
        amount: 1,
        fee_per_byte: None,
        utxos: String::new(),
    })
    .unwrap_err();
    assert!(error.to_string().contains("fee per byte is required"));
}

#[test]
fn burn_and_mine_forms_require_fee_per_byte() {
    let burn = required_fee_per_byte_burn(&super::BurnSettingsForm {
        enabled: Some(true),
        amount: 1,
        fee_per_byte: None,
    })
    .unwrap_err();
    assert!(burn.to_string().contains("fee per byte is required"));
}

#[test]
fn transfer_form_trims_recipient() {
    let (to, amount, fee, utxos) = validate_transfer_form(TransferForm {
        to: "  abc  ".to_string(),
        amount: 2,
        fee_per_byte: Some(3),
        utxos: String::new(),
    })
    .unwrap();

    assert_eq!(to, "abc");
    assert_eq!(amount, 2);
    assert_eq!(fee, 3);
    assert!(utxos.is_empty());
}

#[test]
fn transfer_form_parses_selected_utxos() {
    let (_, _, _, utxos) = validate_transfer_form(TransferForm {
        to: "abc".to_string(),
        amount: 2,
        fee_per_byte: Some(3),
        utxos: "tx-one:0\ntx:with:colons:7,\n".to_string(),
    })
    .unwrap();

    assert_eq!(
        utxos,
        vec![
            OutPoint {
                txid: "tx-one".to_string(),
                index: 0
            },
            OutPoint {
                txid: "tx:with:colons".to_string(),
                index: 7
            }
        ]
    );
}
