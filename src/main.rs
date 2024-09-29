use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_ws::{Message, Session};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
struct Player {
    id: String,
    x: f32,
    y: f32,
}

#[derive(Clone, Serialize, Deserialize)]
struct GameState {
    players: Vec<Player>,
}

#[derive(Serialize, Deserialize)]
enum GameMessage {
    Join(Player),
    Move { id: String, x: f32, y: f32 },
    GameState(GameState),
}

struct AppState {
    game_state: Mutex<GameState>,
    clients: Mutex<Vec<mpsc::UnboundedSender<Message>>>,
}

async fn index() -> HttpResponse {
    HttpResponse::Ok().body(include_str!("../static/index.html"))
}

async fn ws(
    req: HttpRequest,
    stream: web::Payload,
    app_state: web::Data<Arc<AppState>>,
) -> Result<HttpResponse, Error> {
    let (response, session, msg_stream) = actix_ws::handle(&req, stream)?;

    actix_web::rt::spawn(websocket_handler(session, msg_stream, app_state));

    Ok(response)
}

async fn websocket_handler(
    mut session: Session,
    mut msg_stream: actix_ws::MessageStream,
    app_state: web::Data<Arc<AppState>>,
) {
    let (tx, mut rx) = mpsc::unbounded_channel();

    // Add the client to the list
    app_state.clients.lock().await.push(tx.clone());

}

async fn handle_game_message(app_state: &web::Data<Arc<AppState>>, message: GameMessage) {
    match message {
        GameMessage::Move { id, x, y } => {
            let mut game_state = app_state.game_state.lock().await;
            if let Some(player) = game_state.players.iter_mut().find(|p| p.id == id) {
                player.x = x;
                player.y = y;
            }
            broadcast_message(app_state, &GameMessage::GameState(game_state.clone())).await;
        }
        _ => {}
    }
}

async fn broadcast_message(app_state: &web::Data<Arc<AppState>>, message: &GameMessage) {
    let message_str = serde_json::to_string(message).unwrap();
    for client in app_state.clients.lock().await.iter() {
        let _ = client.send(Message::Text(message_str.clone().into()));
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let app_state = Arc::new(AppState {
        game_state: Mutex::new(GameState {
            players: Vec::new(),
        }),
        clients: Mutex::new(Vec::new()),
    });

    log::info!("Starting server at http://localhost:8080");

    HttpServer::new(move || {
        App::new()
            .app_data(web::Data::new(app_state.clone()))
            .route("/", web::get().to(index))
            .route("/ws", web::get().to(ws))
    })
    .bind("127.0.0.1:8080")?
    .run()
    .await
}
