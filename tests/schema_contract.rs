//! Cross-check schema migrations and backfills against the Python reference.
use rusqlite::{types::ValueRef, Connection};
use serde_json::{json, Map, Value};

fn rows(db: &Connection, sql: &str) -> Vec<Value> {
    let mut stmt = db.prepare(sql).unwrap();
    let names = stmt.column_names().map(str::to_owned).collect::<Vec<_>>();
    stmt.query_map([], |row| {
        let mut values = Map::new();
        for (index, name) in names.iter().enumerate() {
            let value = match row.get_ref(index)? {
                ValueRef::Null => Value::Null,
                ValueRef::Integer(v) => json!(v),
                ValueRef::Real(v) => json!(v),
                ValueRef::Text(v) => json!(String::from_utf8_lossy(v)),
                ValueRef::Blob(_) => panic!("unexpected blob"),
            };
            values.insert(name.clone(), value);
        }
        Ok(Value::Object(values))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}
fn snapshot(db: &Connection) -> Value {
    let tables = rows(db, "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name");
    let mut schema = Map::new();
    for row in tables {
        let table = row["name"].as_str().unwrap();
        let mut columns = rows(db, &format!("PRAGMA table_info(\"{table}\")"));
        let mut indexes = rows(db, &format!("PRAGMA index_list(\"{table}\")"));
        for column in &mut columns {
            column.as_object_mut().unwrap().remove("cid");
        }
        for index in &mut indexes {
            let name = index["name"].as_str().unwrap();
            let columns = rows(db, &format!("PRAGMA index_info(\"{name}\")"));
            let names = columns
                .iter()
                .map(|column| column["name"].clone())
                .collect::<Vec<_>>();
            index.as_object_mut().unwrap().remove("seq");
            index
                .as_object_mut()
                .unwrap()
                .insert("columns".into(), json!(names));
        }
        columns.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        indexes.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        schema.insert(table.into(), json!({"columns":columns,"indexes":indexes}));
    }
    let mut contents = Map::new();
    for table in ["users", "trainings", "bookings", "event_notifications"] {
        contents.insert(
            table.into(),
            json!(rows(
                db,
                &format!("SELECT * FROM \"{table}\" ORDER BY rowid")
            )),
        );
    }
    json!({"schema":schema,"rows":contents})
}
#[test]
#[ignore = "requires the Python reference dependencies; set SQASSIT_REFERENCE_PYTHON and run --ignored"]
fn same_fresh_and_old_schema_as_python() {
    let python = std::env::var("SQASSIT_REFERENCE_PYTHON").expect("set SQASSIT_REFERENCE_PYTHON");
    for mode in ["fresh", "old"] {
        let output = std::process::Command::new(&python)
            .args(["tests/schema_oracle.py", mode])
            .output()
            .expect("run schema oracle");
        assert!(
            output.status.success(),
            "Python schema oracle failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: Value = serde_json::from_slice(&output.stdout).unwrap();
        let mut db = Connection::open_in_memory().unwrap();
        if mode == "old" {
            db.execute_batch(include_str!("../src/schema.sql")).unwrap();
            db.execute_batch(include_str!("old_schema_fixture.sql"))
                .unwrap();
        }
        sqassit::storage::initialize(&mut db).unwrap();
        let first = snapshot(&db);
        sqassit::storage::initialize(&mut db).unwrap();
        let second = snapshot(&db);
        assert_eq!(first, second, "{mode} repeated migration");
        assert_eq!(second, expected, "{mode} schema");
    }
}
