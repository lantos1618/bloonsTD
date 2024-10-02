// src/main.rs

use actix_web::{web, App, Error, HttpRequest, HttpResponse, HttpServer};
use actix_ws::{Message, Session};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{self, Duration, Instant};
use uuid::Uuid;
use rand::Rng;

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Player {
    id: String,
    color: String,
    balloon_color: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct Balloon {
    id: String,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    radius: f32,
    health: i32,
    player_id: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
struct GridCell {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    owner: Option<String>, // Player ID
}

#[derive(Clone, Serialize, Debug)]
struct Emitter {
    id: String,
    x: f32,
    y: f32,
    player_id: String,
    #[serde(skip_serializing)]
    last_emit_time: Instant,
    health: i32,
}

#[derive(Clone, Serialize, Debug)]
struct Base {
    x: f32,
    y: f32,
    player_id: String,
    health: i32,
    size: f32,
    #[serde(skip_serializing)]
    last_emit_time: Instant,
}

#[derive(Clone, Serialize, Debug)]
struct GameState {
    players: Vec<Player>,
    balloons: Vec<Balloon>,
    grid: Vec<GridCell>,
    emitters: Vec<Emitter>,
    bases: Vec<Base>,
    #[serde(skip_serializing)]
    last_update: Instant,
    game_over: bool,
    winner_player_id: Option<String>,
    host_id: Option<String>,
    game_started: bool,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "type")]
enum ClientMessage {
    JoinRoom { room_id: String },
    PlaceEmitter { x: f32, y: f32 },
    StartGame,
}

#[derive(Serialize, Debug)]
#[serde(tag = "type")]
enum ServerMessage {
    JoinedRoom { room_id: String, player_id: String },
    GameState(GameState),
    GameOver { winner_color: String },
    PlayerList { players: Vec<Player>, host_id: Option<String> },
}

struct AppState {
    rooms: Mutex<HashMap<String, GameState>>, // Rooms by room ID
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
        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(msg) => msg,
            Err(err) => {
                eprintln!("Failed to parse message: {:?}", err);
                let _ = session.close(None).await;
                return;
            }
        };
        match client_msg {
            ClientMessage::JoinRoom { room_id } => {
                let player_id = Uuid::new_v4().to_string();
                (room_id, player_id)
            }
            _ => {
                // Unexpected message
                let _ = session.close(None).await;
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

    // Assign a color to the player
    let colors = vec![
        ("#8A2BE2".to_string(), "#4B0082".to_string()), // Purple
        ("#32CD32".to_string(), "#006400".to_string()), // Green
        ("#FF0000".to_string(), "#8B0000".to_string()), // Red
        ("#FFA500".to_string(), "#FF8C00".to_string()), // Orange
        ("#0000FF".to_string(), "#00008B".to_string()), // Blue
    ];

    {
        let mut rooms = app_state.rooms.lock().await;
        let room = rooms.entry(room_id.clone()).or_insert_with(|| init_game_state());
        // Assign a color to the player
        let assigned_colors: HashSet<_> = room.players.iter().map(|p| p.color.clone()).collect();
        let default_colors = ("#FF0000".to_string(), "#8B0000".to_string());
        let player_color = colors
            .iter()
            .find(|(color, _)| !assigned_colors.contains(color))
            .unwrap_or(&default_colors);

        let new_player = Player {
            id: player_id.clone(),
            color: player_color.0.clone(),
            balloon_color: player_color.1.clone(),
        };

        room.players.push(new_player);

        // Set the host if not already set
        if room.host_id.is_none() {
            room.host_id = Some(player_id.clone());
        }

        // Initialize base for the new player
        let base_size = 50.0;
        let canvas_width = 800.0;
        let canvas_height = 600.0;

        let base_positions = vec![
            // Positions for up to 4 players
            (base_size / 2.0, canvas_height - base_size / 2.0), // Bottom-left
            (canvas_width - base_size / 2.0, base_size / 2.0), // Top-right
            (base_size / 2.0, base_size / 2.0),                // Top-left
            (canvas_width - base_size / 2.0, canvas_height - base_size / 2.0), // Bottom-right
        ];

        let base_position = base_positions[(room.players.len() - 1) % base_positions.len()];

        let base = Base {
            x: base_position.0,
            y: base_position.1,
            player_id: player_id.clone(),
            health: 100,
            size: base_size,
            last_emit_time: Instant::now(),
        };
        room.bases.push(base);
    }

    // Send JoinedRoom message to client
    let join_msg = ServerMessage::JoinedRoom {
        room_id: room_id.clone(),
        player_id: player_id.clone(),
    };
    let join_msg_str = serde_json::to_string(&join_msg).unwrap();
    let _ = session.text(join_msg_str).await;

    // Send updated PlayerList to clients
    broadcast_player_list(app_state.get_ref().clone(), &room_id).await;

    // Spawn a task to receive messages for this client and send them through the WebSocket
    let mut session_clone = session.clone();
    actix_web::rt::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Message::Text(text) = msg {
                // Set a timeout for the send operation
                if let Err(e) = tokio::time::timeout(Duration::from_secs(5), session_clone.text(text.to_string())).await {
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
            let client_msg: ClientMessage = match serde_json::from_str(&text) {
                Ok(msg) => msg,
                Err(err) => {
                    eprintln!("Failed to parse message: {:?}", err);
                    continue;
                }
            };
            handle_client_message(&app_state, client_msg, room_id.clone(), player_id.clone()).await;
        }
    }

    // Client disconnected, clean up
    {
        let mut rooms = app_state.rooms.lock().await;
        if let Some(room) = rooms.get_mut(&room_id) {
            room.players.retain(|p| p.id != player_id);

            // If the host left, assign a new host
            if room.host_id.as_ref() == Some(&player_id) {
                if let Some(new_host) = room.players.first() {
                    room.host_id = Some(new_host.id.clone());
                } else {
                    room.host_id = None;
                }
            }
        }
    }

    // Remove client from clients list
    {
        let mut clients = app_state.clients.lock().await;
        if let Some(client_list) = clients.get_mut(&room_id) {
            client_list.retain(|client| !client.same_channel(&tx));
        }
    }

    // Send updated PlayerList to clients
    broadcast_player_list(app_state.get_ref().clone(), &room_id).await;
}

async fn handle_client_message(
    app_state: &web::Data<Arc<AppState>>,
    message: ClientMessage,
    room_id: String,
    player_id: String,
) {
    match message {
        ClientMessage::PlaceEmitter { x, y } => {
            let mut rooms = app_state.rooms.lock().await;
            if let Some(room) = rooms.get_mut(&room_id) {
                // Only allow actions if the game has started
                if !room.game_started {
                    return;
                }
                // Find the grid cell at the clicked position
                if let Some(cell) = room.grid.iter_mut().find(|cell| {
                    x >= cell.x && x < cell.x + cell.w && y >= cell.y && y < cell.y + cell.h
                }) {
                    if let Some(owner_id) = &cell.owner {
                        if owner_id == &player_id {
                            // Place an emitter
                            let emitter = Emitter {
                                id: Uuid::new_v4().to_string(),
                                x: cell.x + cell.w / 2.0,
                                y: cell.y + cell.h / 2.0,
                                player_id: player_id.clone(),
                                last_emit_time: Instant::now(),
                                health: 3,
                            };
                            room.emitters.push(emitter);
                        }
                    }
                }
            }
        }
        ClientMessage::StartGame => {
            let mut rooms = app_state.rooms.lock().await;
            if let Some(room) = rooms.get_mut(&room_id) {
                if room.host_id.as_ref() == Some(&player_id) && !room.game_started {
                    // Start the game
                    room.game_started = true;

                    // Start the game loop
                    let app_state_clone = app_state.clone();
                    let room_id_clone = room_id.clone();
                    actix_web::rt::spawn(async move {
                        game_loop(app_state_clone, room_id_clone).await;
                    });

                    // Broadcast updated game state immediately
                    let app_state_clone = app_state.clone();
                    let room_id_clone = room_id.clone();
                    actix_web::rt::spawn(async move {
                        let app_state_arc = app_state_clone.get_ref().clone(); // Extract Arc<AppState> from Data
                        broadcast_game_state(app_state_arc, &room_id_clone).await;
                    });
                }
            }
        }
        _ => {}
    }
}

async fn game_loop(app_state: web::Data<Arc<AppState>>, room_id: String) {
    let mut interval = time::interval(Duration::from_millis(16)); // Approx. 60 FPS
    loop {
        interval.tick().await;

        // Update game state
        let game_over = {
            let mut rooms = app_state.rooms.lock().await;
            if let Some(room) = rooms.get_mut(&room_id) {
                if room.game_over {
                    true
                } else {
                    update_game_state(room);
                    false
                }
            } else {
                // Room no longer exists
                break;
            }
        };

        // Broadcast updated game state
        broadcast_game_state(app_state.get_ref().clone(), &room_id).await;

        if game_over {
            // Send GameOver message to clients
            let winner_color = {
                let rooms = app_state.rooms.lock().await;
                if let Some(room) = rooms.get(&room_id) {
                    if let Some(winner_id) = &room.winner_player_id {
                        if let Some(winner) = room.players.iter().find(|p| &p.id == winner_id) {
                            winner.color.clone()
                        } else {
                            "Unknown".to_string()
                        }
                    } else {
                        "Draw".to_string()
                    }
                } else {
                    "Unknown".to_string()
                }
            };

            broadcast_game_over(app_state.get_ref().clone(), &room_id, winner_color).await;
            break;
        }
    }
}

fn init_game_state() -> GameState {
    let mut game_state = GameState {
        players: Vec::new(),
        balloons: Vec::new(),
        grid: Vec::new(),
        emitters: Vec::new(),
        bases: Vec::new(),
        last_update: Instant::now(),
        game_over: false,
        winner_player_id: None,
        host_id: None,
        game_started: false,
    };

    // Initialize grid
    let grid_rows = 20;
    let grid_cols = 20;
    let canvas_width = 800.0;
    let canvas_height = 600.0;
    let grid_row_size = canvas_width / grid_rows as f32;
    let grid_col_size = canvas_height / grid_cols as f32;

    for i in 0..grid_rows {
        for j in 0..grid_cols {
            game_state.grid.push(GridCell {
                x: i as f32 * grid_row_size,
                y: j as f32 * grid_col_size,
                w: grid_row_size,
                h: grid_col_size,
                owner: None,
            });
        }
    }

    game_state
}

fn update_game_state(game_state: &mut GameState) {
    if !game_state.game_started {
        return;
    }

    let now = Instant::now();
    let delta_time = now.duration_since(game_state.last_update).as_secs_f32();
    game_state.last_update = now;

    // Update balloons
    for balloon in &mut game_state.balloons {
        balloon.x += balloon.vx * delta_time;
        balloon.y += balloon.vy * delta_time;

        // Handle canvas boundaries
        let canvas_width = 800.0;
        let canvas_height = 600.0;

        if balloon.x - balloon.radius < 0.0 || balloon.x + balloon.radius > canvas_width {
            balloon.vx *= -1.0;
        }
        if balloon.y - balloon.radius < 0.0 || balloon.y + balloon.radius > canvas_height {
            balloon.vy *= -1.0;
        }
    }

    // Update grid cell ownership
    for balloon in &game_state.balloons {
        if let Some(cell) = game_state.grid.iter_mut().find(|cell| {
            balloon.x >= cell.x && balloon.x < cell.x + cell.w &&
            balloon.y >= cell.y && balloon.y < cell.y + cell.h
        }) {
            cell.owner = Some(balloon.player_id.clone());
        }
    }

    // Handle collisions
    handle_collisions(game_state);

    // Update emitters
    for emitter in &mut game_state.emitters {
        if now.duration_since(emitter.last_emit_time).as_secs_f32() > 2.0 {
            // Emit a balloon
            let mut rng = rand::thread_rng();
            let angle = rng.gen_range(0.0..std::f32::consts::TAU);
            let speed = 100.0 + rng.gen_range(0.0..100.0);
            let vx = angle.cos() * speed;
            let vy = angle.sin() * speed;

            let balloon = Balloon {
                id: Uuid::new_v4().to_string(),
                x: emitter.x,
                y: emitter.y,
                vx,
                vy,
                radius: 10.0,
                health: 3,
                player_id: emitter.player_id.clone(),
            };
            game_state.balloons.push(balloon);

            emitter.last_emit_time = now;
        }
    }

    // Update bases
    for base in &mut game_state.bases {
        if now.duration_since(base.last_emit_time).as_secs_f32() > 2.0 {
            // Emit a balloon
            let mut rng = rand::thread_rng();
            let angle = rng.gen_range(0.0..std::f32::consts::TAU);
            let speed = 100.0 + rng.gen_range(0.0..100.0);
            let vx = angle.cos() * speed;
            let vy = angle.sin() * speed;

            let balloon = Balloon {
                id: Uuid::new_v4().to_string(),
                x: base.x,
                y: base.y,
                vx,
                vy,
                radius: 10.0,
                health: 3,
                player_id: base.player_id.clone(),
            };
            game_state.balloons.push(balloon);

            base.last_emit_time = now;
        }
    }
}

fn handle_collisions(game_state: &mut GameState) {
    let mut emitters_to_remove: Vec<String> = Vec::new(); // IDs of emitters to remove
    let mut bases_to_remove: Vec<String> = Vec::new();    // Player IDs whose bases are destroyed

    // Balloon vs. Balloon collisions
    for i in 0..game_state.balloons.len() {
        for j in (i + 1)..game_state.balloons.len() {
            let (balloon_a, balloon_b) = {
                let (left, right) = game_state.balloons.split_at_mut(j);
                (&mut left[i], &mut right[0])
            };

            if balloon_a.player_id != balloon_b.player_id && check_collision(balloon_a, balloon_b) {
                // Both balloons take damage
                balloon_a.health -= 1;
                balloon_b.health -= 1;

                // Swap velocities to simulate bounce
                std::mem::swap(&mut balloon_a.vx, &mut balloon_b.vx);
                std::mem::swap(&mut balloon_a.vy, &mut balloon_b.vy);
            }
        }
    }

    // Balloon vs. Emitter collisions
    for balloon in &mut game_state.balloons {
        for emitter in &mut game_state.emitters {
            if balloon.player_id != emitter.player_id && circle_collision(balloon.x, balloon.y, balloon.radius, emitter.x, emitter.y, 15.0) {
                // Balloon and emitter take damage
                balloon.health -= 1;
                emitter.health -= 1;
                if emitter.health <= 0 {
                    emitters_to_remove.push(emitter.id.clone());
                }
            }
        }
    }

    // Balloon vs. Base collisions
    for balloon in &mut game_state.balloons {
        for base in &mut game_state.bases {
            if balloon.player_id != base.player_id && rect_collision(balloon, base) {
                // Balloon and base take damage
                balloon.health -= 1;
                base.health -= 1;
                if base.health <= 0 {
                    bases_to_remove.push(base.player_id.clone());
                }
            }
        }
    }

    // Remove destroyed balloons
    game_state.balloons.retain(|balloon| balloon.health > 0);

    // Remove destroyed emitters
    game_state.emitters.retain(|emitter| !emitters_to_remove.contains(&emitter.id));

    // Remove destroyed bases and players
    for destroyed_player_id in bases_to_remove {
        // Remove the base
        if let Some(index) = game_state.bases.iter().position(|b| b.player_id == destroyed_player_id) {
            game_state.bases.remove(index);
        }

        // Remove the player
        game_state.players.retain(|p| p.id != destroyed_player_id);
    }

    // Check for game over
    if game_state.bases.len() <= 1 {
        game_state.game_over = true;
        if let Some(winner_base) = game_state.bases.first() {
            game_state.winner_player_id = Some(winner_base.player_id.clone());
        }
    }
}

fn check_collision(balloon_a: &Balloon, balloon_b: &Balloon) -> bool {
    let dx = balloon_a.x - balloon_b.x;
    let dy = balloon_a.y - balloon_b.y;
    let distance = (dx * dx + dy * dy).sqrt();
    distance < balloon_a.radius + balloon_b.radius
}

fn circle_collision(x1: f32, y1: f32, r1: f32, x2: f32, y2: f32, r2: f32) -> bool {
    let dx = x1 - x2;
    let dy = y1 - y2;
    let distance = (dx * dx + dy * dy).sqrt();
    distance < r1 + r2
}

fn rect_collision(balloon: &Balloon, base: &Base) -> bool {
    let half_size = base.size / 2.0;
    let left = base.x - half_size;
    let right = base.x + half_size;
    let top = base.y - half_size;
    let bottom = base.y + half_size;

    balloon.x + balloon.radius > left &&
    balloon.x - balloon.radius < right &&
    balloon.y + balloon.radius > top &&
    balloon.y - balloon.radius < bottom
}

async fn broadcast_game_state(app_state: Arc<AppState>, room_id: &String) {
    // Retrieve room data without holding the lock during sending
    let game_state = {
        let rooms = app_state.rooms.lock().await;
        if let Some(room) = rooms.get(room_id) {
            room.clone()
        } else {
            // Room not found
            return;
        }
    };

    let message = ServerMessage::GameState(game_state);

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

async fn broadcast_game_over(app_state: Arc<AppState>, room_id: &String, winner_color: String) {
    let message = ServerMessage::GameOver { winner_color };

    let message_str = serde_json::to_string(&message).unwrap();

    let clients = {
        let clients_lock = app_state.clients.lock().await;
        if let Some(clients) = clients_lock.get(room_id) {
            clients.clone()
        } else {
            // No clients in this room
            return;
        }
    };

    // Send the game over message to each client
    for client in clients.iter() {
        let _ = client.send(Message::Text(message_str.clone().into()));
    }
}

async fn broadcast_player_list(app_state: Arc<AppState>, room_id: &String) {
    let (players, host_id) = {
        let rooms = app_state.rooms.lock().await;
        if let Some(room) = rooms.get(room_id) {
            (room.players.clone(), room.host_id.clone())
        } else {
            return;
        }
    };

    let message = ServerMessage::PlayerList { players, host_id };

    let message_str = serde_json::to_string(&message).unwrap();

    let clients = {
        let clients_lock = app_state.clients.lock().await;
        if let Some(clients) = clients_lock.get(room_id) {
            clients.clone()
        } else {
            return;
        }
    };

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
            .service(actix_files::Files::new("/static", "./static"))
    })
    .bind("0.0.0.0:5000")?
    .run()
    .await
}