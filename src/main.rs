// src/main.rs

use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_ws::{Message, Session};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use uuid::Uuid;
use tokio::time::{timeout, Duration};

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
    // Client sends this to join a room
    JoinRoom { room_id: String },
    // Server responds with this after assigning a player_id
    JoinedRoom { room_id: String, player_id: String },
    // Client sends this to move
    Move { x: f32, y: f32 },
    // Server sends this to update game state
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
    // Wait for JoinRoom message
    let first_message = msg_stream.next().await;

    let (room_id, player_id) = if let Some(Ok(Message::Text(text))) = first_message {
        let game_msg: GameMessage = serde_json::from_str(&text).unwrap();
        match game_msg {
            GameMessage::JoinRoom { room_id } => {
                let player_id = Uuid::new_v4().to_string();
                (room_id, player_id)
            }
            _ => {
                // Unexpected message
                session.close(None).await.unwrap();
                return;
            }
        }
    } else {
        // Connection closed or invalid message
        return;
    };

    let (tx, mut rx) = mpsc::unbounded_channel();
    app_state
        .clients
        .lock()
        .await
        .entry(room_id.clone())
        .or_insert_with(Vec::new)
        .push(tx.clone());

    // Send JoinedRoom message to client
    let join_msg = GameMessage::JoinedRoom {
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
                // Set a timeout for the send operation
                if let Err(e) = timeout(Duration::from_secs(5), session_clone.text(text.to_string())).await {
                    eprintln!("Failed to send message to client within timeout: {:?}", e);
                    // Optionally, you can close the session here if needed
                    let _ = session_clone.close(None).await;
                    break;
                }
            }
        }
    });

    while let Some(Ok(msg)) = msg_stream.next().await {
        if let Message::Text(text) = msg {
            let game_msg: GameMessage = serde_json::from_str(&text).unwrap();
            println!("< {:?}", game_msg);
            handle_game_message(&app_state, game_msg, room_id.clone(), player_id.clone()).await;
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
    player_id: String,
) {
    match message {
        GameMessage::Move { x, y } => {
            // Update the player's position
            {
                let mut rooms = app_state.rooms.lock().await;
                if let Some(room) = rooms.get_mut(&room_id) {
                    if let Some(player) = room.players.iter_mut().find(|p| p.id == player_id) {
                        player.x = x;
                        player.y = y;
                    }
                }
            } // Lock is released here

            // Now broadcast the updated game state
            broadcast_game_state(app_state.get_ref().clone(), &room_id).await;
        }
        _ => {}
    }
}

async fn broadcast_game_state(app_state: Arc<AppState>, room_id: &String) {
    // Retrieve room data without holding the lock during sending
    let (players, balls, emitters, bases) = {
        let rooms = app_state.rooms.lock().await;
        if let Some(room) = rooms.get(room_id) {
            (
                room.players.clone(),
                room.balls.clone(),
                room.emitters.clone(),
                room.bases.clone(),
            )
        } else {
            // Room not found
            return;
        }
    };

    let message = GameMessage::GameState {
        players,
        balls,
        emitters,
        bases,
    };

    let message_str = serde_json::to_string(&message).unwrap(); // Serialize to JSON

    // Retrieve clients without holding the lock during sending
    let clients = {
        let clients_lock = app_state.clients.lock().await;
        if let Some(clients) = clients_lock.get(room_id) {
            clients.clone()
        } else {
            // No clients in this room
            return;
        }
    };

    // Now send the message to each client without holding any locks
    for client in clients.iter() {
        let _ = client.send(Message::Text(message_str.clone().into()));
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
