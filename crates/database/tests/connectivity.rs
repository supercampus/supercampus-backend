use supercampus_database::Database;

#[tokio::test]
#[ignore = "requires DATABASE_URL"]
async fn supplied_database_is_reachable() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL is required");
    let database = Database::connect(&url).await.expect("database connection");
    database.ping().await.expect("database ping");

    let database_name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(database.pool())
        .await
        .expect("read current database");
    let schemas: Vec<String> = sqlx::query_scalar(
        "SELECT schema_name FROM information_schema.schemata ORDER BY schema_name",
    )
    .fetch_all(database.pool())
    .await
    .expect("list schemas");

    assert!(!database_name.is_empty());
    eprintln!("connected database with schemas: {}", schemas.join(", "));
}
