// src/main.rs

use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_ws::{Message, Session};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;


#[derive(Clone, Serialize, Deserialize, Debug)]
struct Player {
    id: String,
    color: String,
    ball_color: String,
    x: f32,
    y: f32,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Ball {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    radius: f32,
    player_id: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Emitter {
    x: f32,
    y: f32,
    player_id: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Base {
    x: f32,
    y: f32,
    player_id: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct GameRoom {
    players: Vec<Player>,
    balls: Vec<Ball>,
    emitters: Vec<Emitter>,
    bases: Vec<Base>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
enum GameMessage {
    JoinRoom { room_id: String, player_id: String },
    Move { id: String, x: f32, y: f32 },
    GameState {
        players: Vec<Player>,
        balls: Vec<Ball>,
        emitters: Vec<Emitter>,
        bases: Vec<Base>,
    },
}

struct AppState {
    rooms: Mutex<HashMap<String, GameRoom>>, // Rooms by room ID
    clients: Mutex<HashMap<String, Vec<mpsc::UnboundedSender<Message>>>>, // Clients by room ID
}

async fn index() -> HttpResponse {
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(include_str!("../static/index.html"))
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
    let player_id = Uuid::new_v4().to_string();
    let room_id = "default_room".to_string(); // For now, using a default room

    let (tx, mut rx) = mpsc::unbounded_channel();
    app_state
        .clients
        .lock()
        .await
        .entry(room_id.clone())
        .or_insert_with(Vec::new)
        .push(tx.clone());

    // Send JoinRoom message to client
    let join_msg = GameMessage::JoinRoom {
        room_id: room_id.clone(),
        player_id: player_id.clone(),
    };
    let join_msg_str = serde_json::to_string(&join_msg).unwrap();
    session.text(join_msg_str).await.unwrap();

    let new_player = Player {
        id: player_id.clone(),
        color: "#8A2BE2".to_string(),
        ball_color: "#4B0082".to_string(),
        x: 50.0,
        y: 50.0,
    };

    {
        let mut rooms = app_state.rooms.lock().await;
        let room = rooms.entry(room_id.clone()).or_insert_with(|| GameRoom {
            players: Vec::new(),
            balls: Vec::new(),
            emitters: Vec::new(),
            bases: Vec::new(),
        });
        room.players.push(new_player);
    }

    broadcast_game_state(app_state.get_ref().clone(), &room_id).await;

    // Spawn a task to receive messages for this client and send them through the WebSocket
    let mut session_clone = session.clone();
    actix_web::rt::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Message::Text(text) = msg {
                // Convert ByteString to String using to_string_lossy() to handle non-UTF-8
                session_clone.text(text.to_string()).await.unwrap();
            }
        }
    });
    
    

    while let Some(Ok(msg)) = msg_stream.next().await {
        if let Message::Text(text) = msg {
            let game_msg: GameMessage = serde_json::from_str(&text).unwrap();
            println!("< {:?}", game_msg);
            handle_game_message(&app_state, game_msg, room_id.clone()).await;
        }
    }

    // Client disconnected, clean up
    {
        let mut rooms = app_state.rooms.lock().await;
        if let Some(room) = rooms.get_mut(&room_id) {
            if let Some(pos) = room.players.iter().position(|p| p.id == player_id) {
                room.players.remove(pos);
            }
        }
    }

    {
        let mut clients = app_state.clients.lock().await;
        if let Some(client_list) = clients.get_mut(&room_id) {
            client_list.retain(|client| !client.same_channel(&tx));
        }
    }

    broadcast_game_state(app_state.get_ref().clone(), &room_id).await;
}

async fn handle_game_message(
    app_state: &web::Data<Arc<AppState>>,
    message: GameMessage,
    room_id: String,
) {
    match message {
        GameMessage::Move { id, x, y } => {
            let mut rooms = app_state.rooms.lock().await;
            if let Some(room) = rooms.get_mut(&room_id) {
                if let Some(player) = room.players.iter_mut().find(|p| p.id == id) {
                    player.x = x;
                    player.y = y;
                }
            }
            broadcast_game_state(app_state.get_ref().clone(), &room_id).await;
        }
        _ => {}
    }
}

async fn broadcast_game_state(app_state: Arc<AppState>, room_id: &String) {
    let rooms = app_state.rooms.lock().await;
    if let Some(room) = rooms.get(room_id) {
        let message = GameMessage::GameState {
            players: room.players.clone(),
            balls: room.balls.clone(),
            emitters: room.emitters.clone(),
            bases: room.bases.clone(),
        };

        let message_str = serde_json::to_string(&message).unwrap(); // Serialize to JSON

        if let Some(clients) = app_state.clients.lock().await.get(room_id) {
            for client in clients.iter() {
                let _ = client.send(Message::Text(message_str.clone().into()));
            }
        }
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init_from_env(env_logger::Env::new().default_filter_or("info"));

    let app_state = Arc::new(AppState {
        rooms: Mutex::new(HashMap::new()),
        clients: Mutex::new(HashMap::new()),
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
