// Test-only fault injection: fail a logical table write, not one SQL verb.
pub fn fail_writes(db: &rusqlite::Connection, table: &str, message: &str) {
    for operation in ["INSERT", "UPDATE", "DELETE"] {
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_{table}_{operation} BEFORE {operation} ON {table} BEGIN SELECT RAISE(FAIL,'{message}'); END;"
        )).unwrap();
    }
}

pub fn allow_writes(db: &rusqlite::Connection, table: &str) {
    for operation in ["INSERT", "UPDATE", "DELETE"] {
        db.execute_batch(&format!("DROP TRIGGER fail_{table}_{operation};"))
            .unwrap();
    }
}
