#[test]
fn fts5_is_available() {
    let c = rusqlite::Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE VIRTUAL TABLE t USING fts5(body);
         INSERT INTO t(body) VALUES ('tawny steller invoice');",
    )
    .expect("FTS5 must be compiled into the bundled SQLite");
    let n: i64 = c
        .query_row("SELECT count(*) FROM t WHERE t MATCH 'steller'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}
