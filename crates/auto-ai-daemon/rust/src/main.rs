use axum::Router;
use axum::routing::get;
use axum::serve;
use tokio::net::TcpListener;

async fn health() -> String {
    "ok".to_string()
}

pub fn build_app() -> Router {
    Router::new().route("/health", get(health))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();
    let listener = TcpListener::bind("127.0.0.1:17699").await.unwrap();
    println!("spike daemon on :17699");
    serve(listener, build_app()).await.unwrap();
}
