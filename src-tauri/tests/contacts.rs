//! The address book derived from the store — `db::queries::address_book`.
//!
//! The thing under test is a ranking, so most of these assert an *order*
//! rather than a membership: the composer offers six rows, and which six is
//! the whole feature. The rule being pinned down is "people you write to beat
//! people who merely write to you, and recency breaks the tie".

use mach_lib::db::models::*;
use mach_lib::db::queries as q;
use mach_lib::db::Db;

// ---------------------------------------------------------------------------
// harness
// ---------------------------------------------------------------------------

fn account(db: &Db, email: &str) -> i64 {
    let conn = db.writer();
    q::upsert_account(
        &conn,
        &NewAccount {
            email: email.to_string(),
            display_name: Some("Me".into()),
            token_ref: format!("keychain:{email}"),
            colour_index: 0,
        },
    )
    .expect("upsert account")
}

fn thread(db: &Db, account_id: i64, gmail_id: &str) -> i64 {
    let conn = db.writer();
    q::upsert_thread(
        &conn,
        &NewThread {
            account_id,
            gmail_thread_id: gmail_id.to_string(),
            participants: Vec::new(),
            subject: "subject".into(),
            snippet: "snippet".into(),
            last_message_at: 0,
            is_unread: false,
            message_count: 1,
            has_attachments: false,
            label_ids: vec!["INBOX".to_string()],
        },
    )
    .expect("upsert thread")
}

fn who(name: Option<&str>, email: &str) -> Participant {
    Participant {
        name: name.map(|n| n.to_string()),
        email: email.to_string(),
    }
}

/// One message, from `from` to `to`, at `at`.
fn message(db: &Db, thread_id: i64, account_id: i64, id: &str, from: Participant, to: Vec<Participant>, at: i64) {
    send(db, thread_id, account_id, id, from, to, Vec::new(), Vec::new(), at, false);
}

#[allow(clippy::too_many_arguments)]
fn send(
    db: &Db,
    thread_id: i64,
    account_id: i64,
    id: &str,
    from: Participant,
    to: Vec<Participant>,
    cc: Vec<Participant>,
    bcc: Vec<Participant>,
    at: i64,
    is_draft: bool,
) {
    let conn = db.writer();
    q::upsert_message(
        &conn,
        &NewMessage {
            thread_id,
            account_id,
            gmail_message_id: id.to_string(),
            from,
            to,
            cc,
            bcc,
            subject: "subject".into(),
            snippet: "snippet".into(),
            internal_date: at,
            is_draft,
            ..Default::default()
        },
    )
    .expect("upsert message");
}

fn book(db: &Db) -> Vec<Contact> {
    let conn = db.writer();
    q::address_book(&conn, q::MAX_CONTACTS).expect("address book")
}

fn find<'a>(book: &'a [Contact], email: &str) -> &'a Contact {
    book.iter()
        .find(|c| c.email == email)
        .unwrap_or_else(|| panic!("{email} is not in the address book: {book:?}"))
}

fn order(book: &[Contact]) -> Vec<&str> {
    book.iter().map(|c| c.email.as_str()).collect()
}

// ---------------------------------------------------------------------------
// ranking
// ---------------------------------------------------------------------------

#[test]
fn someone_you_write_to_outranks_someone_who_only_writes_to_you() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let t = thread(&db, me, "t1");

    // Two messages out to Ada, and forty in from a newsletter that is far more
    // recent. Ada still comes first: `sends` is the only signal that says the
    // owner chose an address.
    message(&db, t, me, "m1", who(Some("Me"), "me@example.com"), vec![who(Some("Ada"), "ada@example.com")], 100);
    message(&db, t, me, "m2", who(Some("Me"), "me@example.com"), vec![who(Some("Ada"), "ada@example.com")], 200);
    for i in 0..40 {
        message(&db, t, me, &format!("n{i}"), who(Some("Deals"), "deals@shop.example"), vec![], 9_000 + i);
    }

    let book = book(&db);
    assert_eq!(order(&book)[0], "ada@example.com");
    assert_eq!(find(&book, "ada@example.com").sends, 2);
    assert_eq!(find(&book, "deals@shop.example").sends, 0);
}

#[test]
fn more_sends_wins_and_recency_breaks_the_tie() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let t = thread(&db, me, "t1");
    let mine = || who(Some("Me"), "me@example.com");

    message(&db, t, me, "m1", mine(), vec![who(None, "often@example.com")], 10);
    message(&db, t, me, "m2", mine(), vec![who(None, "often@example.com")], 20);
    message(&db, t, me, "m3", mine(), vec![who(None, "recent@example.com")], 900);
    message(&db, t, me, "m4", mine(), vec![who(None, "stale@example.com")], 5);
    // Never written to, however recently they showed up.
    message(&db, t, me, "m5", who(None, "seen@example.com"), vec![], 99_999);

    assert_eq!(
        order(&book(&db)),
        vec![
            "often@example.com",  // 2 sends
            "recent@example.com", // 1 send, newest
            "stale@example.com",  // 1 send, oldest
            "seen@example.com",   // no sends, whatever its recency
            "me@example.com",     // self, always last
        ]
    );
}

#[test]
fn cc_and_bcc_count_as_writing_to_someone() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let t = thread(&db, me, "t1");
    send(
        &db,
        t,
        me,
        "m1",
        who(Some("Me"), "me@example.com"),
        vec![who(None, "to@example.com")],
        vec![who(None, "cc@example.com")],
        vec![who(None, "bcc@example.com")],
        50,
        false,
    );

    let book = book(&db);
    assert_eq!(find(&book, "to@example.com").sends, 1);
    assert_eq!(find(&book, "cc@example.com").sends, 1);
    assert_eq!(find(&book, "bcc@example.com").sends, 1);
}

#[test]
fn recipients_of_a_message_you_did_not_send_are_not_sends() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let t = thread(&db, me, "t1");
    // A list blast: the sender is worth completing, the four hundred other
    // people cc'd on it are not evidence of anything and are not collected.
    message(
        &db,
        t,
        me,
        "m1",
        who(Some("List"), "list@example.com"),
        vec![who(None, "stranger@example.com")],
        70,
    );

    let book = book(&db);
    assert_eq!(find(&book, "list@example.com").sends, 0);
    assert!(
        book.iter().all(|c| c.email != "stranger@example.com"),
        "a fellow recipient is not a contact: {book:?}"
    );
}

#[test]
fn a_draft_is_not_a_send() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let t = thread(&db, me, "t1");
    send(
        &db,
        t,
        me,
        "m1",
        who(Some("Me"), "me@example.com"),
        vec![who(None, "halftyped@example.com")],
        Vec::new(),
        Vec::new(),
        50,
        true,
    );

    assert_eq!(order(&book(&db)), vec!["me@example.com"]);
}

// ---------------------------------------------------------------------------
// folding
// ---------------------------------------------------------------------------

#[test]
fn addresses_are_one_contact_whatever_their_case() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let t = thread(&db, me, "t1");
    let mine = || who(Some("Me"), "ME@Example.com");

    message(&db, t, me, "m1", mine(), vec![who(Some("Ada"), "Ada@Example.COM")], 100);
    message(&db, t, me, "m2", mine(), vec![who(None, "ada@example.com")], 300);
    message(&db, t, me, "m3", who(None, "ADA@EXAMPLE.COM"), vec![], 200);

    let book = book(&db);
    assert_eq!(book.iter().filter(|c| c.email == "ada@example.com").count(), 1);
    let ada = find(&book, "ada@example.com");
    assert_eq!(ada.sends, 2);
    assert_eq!(ada.last_seen, 300);
    assert_eq!(ada.name.as_deref(), Some("Ada"));
    // The owner's own address is one contact too, however he capitalised it.
    assert_eq!(book.iter().filter(|c| c.is_self).count(), 1);
}

#[test]
fn a_missing_name_never_unlearns_one_and_the_newest_name_wins() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let t = thread(&db, me, "t1");
    let mine = || who(Some("Me"), "me@example.com");

    // Named, then bare, then renamed. Gmail hands back all three shapes.
    message(&db, t, me, "m1", mine(), vec![who(Some("Ada Lovelace"), "ada@example.com")], 100);
    message(&db, t, me, "m2", mine(), vec![who(None, "ada@example.com")], 200);
    message(&db, t, me, "m3", mine(), vec![who(Some(""), "ada@example.com")], 250);
    message(&db, t, me, "m4", mine(), vec![who(Some("Ada Byron"), "ada@example.com")], 300);
    message(&db, t, me, "m5", mine(), vec![who(None, "ada@example.com")], 400);
    // A "name" that is the address again is not a name.
    message(&db, t, me, "m6", mine(), vec![who(Some("bare@example.com"), "bare@example.com")], 100);

    let book = book(&db);
    assert_eq!(find(&book, "ada@example.com").name.as_deref(), Some("Ada Byron"));
    assert_eq!(find(&book, "ada@example.com").last_seen, 400);
    assert_eq!(find(&book, "bare@example.com").name, None);
}

#[test]
fn an_address_with_no_name_anywhere_is_still_a_contact() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let t = thread(&db, me, "t1");
    message(&db, t, me, "m1", who(Some("Me"), "me@example.com"), vec![who(None, "nameless@example.com")], 42);

    let contact = book(&db);
    let nameless = find(&contact, "nameless@example.com");
    assert_eq!(nameless.name, None);
    assert_eq!(nameless.sends, 1);
    assert_eq!(nameless.last_seen, 42);
}

#[test]
fn a_blank_address_is_not_a_contact() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let t = thread(&db, me, "t1");
    message(&db, t, me, "m1", who(Some("Nobody"), ""), vec![who(Some("Also nobody"), "  ")], 10);

    assert_eq!(order(&book(&db)), vec!["me@example.com"]);
}

// ---------------------------------------------------------------------------
// self, and the cap
// ---------------------------------------------------------------------------

#[test]
fn your_own_addresses_are_marked_and_kept() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let other = account(&db, "second@example.com");
    let t = thread(&db, me, "t1");
    // Mailing yourself is a real thing people do.
    message(&db, t, me, "m1", who(Some("Me"), "me@example.com"), vec![who(None, "me@example.com")], 100);
    let _ = other;

    let book = book(&db);
    assert!(find(&book, "me@example.com").is_self);
    // An account that has never appeared in a message is still in the book —
    // the same rule `contactsFrom` follows for `sources.accounts`.
    assert!(find(&book, "second@example.com").is_self);
    // Marked, not dropped, and last: an address field whose top hit for "m" is
    // your own address is useless.
    assert_eq!(order(&book).last(), Some(&"second@example.com"));
    assert!(book.iter().rev().take(2).all(|c| c.is_self));
}

#[test]
fn the_cap_trims_the_least_useful_and_never_your_own_accounts() {
    let db = Db::open_in_memory().unwrap();
    let me = account(&db, "me@example.com");
    let t = thread(&db, me, "t1");
    for i in 0..10 {
        message(&db, t, me, &format!("in{i}"), who(None, &format!("s{i}@example.com")), vec![], 1_000 + i);
    }
    message(&db, t, me, "out", who(Some("Me"), "me@example.com"), vec![who(None, "ada@example.com")], 1);

    let conn = db.writer();
    let capped = q::address_book(&conn, 3).expect("address book");
    assert_eq!(
        order(&capped),
        vec![
            "ada@example.com", // the one send, however old
            "s9@example.com",  // then the newest sightings
            "s8@example.com",
            "me@example.com", // self is exempt from the cap
        ]
    );
}

#[test]
fn an_empty_store_has_an_empty_address_book() {
    let db = Db::open_in_memory().unwrap();
    assert!(book(&db).is_empty());
}
