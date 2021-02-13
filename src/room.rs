use std::collections::HashMap;

use log::{error, warn};
use rand::distributions::Uniform;
use rand::Rng;
use serde::{Deserialize, Serialize};

use uactor::blocking::{Actor, ActorContext, Addr, Context};

use crate::player::PlayerAddr;

#[derive(Debug)]
pub enum RoomMessage {
    JoinRequest(String, PlayerAddr),
    PlayerLeft(String),
    PlayerVoted(String, Option<String>),
    UpdatePlayer {
        id: String,
        voter: bool,
        name: Option<String>,
    },
    ForceOpen,
    Restart,
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum RejectReason {
    RoomDoesNotExist,
    CreateGameError,
    JoinGameError,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GameState {
    deck: String,
    open: bool,
    votes: HashMap<String, Option<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    id: String,
    name: Option<String>,
    voter: bool,
}

#[derive(Debug, Clone)]
pub enum GamePlayerMessage {
    // join mgmt
    Welcome(String, RoomAddr, GameState, Vec<PlayerState>),
    Rejected(RejectReason),

    // room state sync
    PlayerJoined(PlayerState),
    PlayerChanged(PlayerState),
    PlayerLeft(String),
    GameStateChanged(GameState),
}

#[derive(Clone)]
struct GamePlayer {
    addr: PlayerAddr,

    id: String,
    voter: bool,
    vote: Option<String>,
    name: Option<String>,
}

impl GamePlayer {
    pub fn new(id: &str, addr: PlayerAddr, voter: bool) -> Self {
        Self {
            id: id.to_string(),
            addr,
            voter,
            vote: None,
            name: None,
        }
    }

    fn to_state(&self) -> PlayerState {
        PlayerState {
            id: self.id.clone(),
            name: self.name.clone(),
            voter: self.voter,
        }
    }
}

pub struct Room {
    id: String,
    deck: String,
    players: HashMap<String, GamePlayer>,
    open: bool,
}

impl Room {
    pub fn new(id: &str, creator: (String, PlayerAddr), deck: String) -> Self {
        warn!("{}: Created room", id);
        let mut players = HashMap::new();
        let game_player = GamePlayer::new(&creator.0, creator.1, false);
        players.insert(creator.0, game_player);

        Self {
            id: id.to_string(),
            players,
            open: false,
            deck,
        }
    }

    pub fn gen_id(digits: u8) -> String {
        rand::thread_rng()
            .sample(&Uniform::from(0..10u32.pow(digits as u32)))
            .to_string()
    }

    async fn send_to_player(&mut self, player: &GamePlayer, msg: GamePlayerMessage) {
        let result = player.addr.send(msg).await;
        if result.is_err() {
            error!(
                "{}: Failed to send message to player {}",
                self.id, player.id
            );
            // TODO: self.remove_player(player.)
        }
    }

    async fn send_to_players(&mut self, msg: GamePlayerMessage) {
        for player in self.players.values_mut() {
            let result = player.addr.send(msg.clone()).await;
            if result.is_err() {
                error!(
                    "{}: Failed to send message to player {}",
                    self.id, player.id
                );
                // TODO: self.remove_player(player.)
            }
        }
    }

    async fn add_player(&mut self, player_id: String, player: PlayerAddr, ctx: &Context<Self>) {
        let game_player = GamePlayer::new(&player_id, player.clone(), true);
        let game_player_state = game_player.to_state();
        self.players.insert(player_id, game_player.clone());

        // welcome
        let players_state = self.players.values().map(|p| p.to_state()).collect();
        self.send_to_player(
            &game_player,
            GamePlayerMessage::Welcome(self.id.clone(), ctx.addr(), self.to_state(), players_state),
        )
        .await;

        // introduce
        self.send_to_players(GamePlayerMessage::PlayerJoined(game_player_state.clone()))
            .await;
    }

    async fn remove_player(&mut self, player_id: &str) {
        self.players.remove(player_id);

        // announce
        self.send_to_players(GamePlayerMessage::PlayerLeft(player_id.to_string()))
            .await;

        if self.players.is_empty() {
            warn!("{}: room is now empty", self.id);
            // TODO: remove room in 1min
        }
    }

    async fn set_vote(&mut self, player_id: &str, vote: Option<String>) {
        if let Some(mut player) = self.players.get_mut(player_id) {
            if player.voter {
                player.vote = vote;
            } else {
                warn!("{}: Non-voter {} voted", self.id, player_id);
                return;
            }
        } else {
            warn!(
                "{}: Failed to set vote for non-existing player {}",
                self.id, player_id
            );
            return;
        }

        // TODO: open if everyone has voted
        let all_voted = self
            .players
            .values()
            .all(|player| player.vote.is_some() || !player.voter);
        if all_voted {
            // at least 2 voter must exist to make sense
            let voters = self.players.values().filter(|player| player.voter).count();
            if voters > 1 {
                self.open = true;
            }
        }

        self.send_game_state().await;
    }

    async fn send_game_state(&mut self) {
        self.send_to_players(GamePlayerMessage::GameStateChanged(self.to_state()))
            .await;
    }

    async fn force_open(&mut self) {
        if !self.open {
            self.open = true;
            self.send_game_state().await;
        }
    }

    async fn restart(&mut self) {
        self.open = false;
        for player in self.players.values_mut() {
            player.vote = None;
        }
        self.send_game_state().await;
    }

    async fn update_player(&mut self, id: &str, name: Option<String>, voter: bool) {
        if let Some(player) = self.players.get_mut(id) {
            player.voter = voter;
            player.name = name;

            let state = player.to_state();
            self.send_to_players(GamePlayerMessage::PlayerChanged(state))
                .await;
        } else {
            warn!("{}: Ignoring update on unknown player {}", self.id, id);
        }
    }

    fn to_state(&self) -> GameState {
        GameState {
            deck: self.deck.clone(),
            open: self.open,
            votes: self
                .players
                .values()
                .filter(|p| p.voter)
                .map(|p| {
                    let vote = if self.open {
                        p.vote.clone()
                    } else {
                        p.vote.as_ref().map(|_vote| "�".to_string())
                    };

                    (p.id.clone(), vote)
                })
                .collect(),
        }
    }
}

pub type RoomAddr = Addr<RoomMessage>;

#[async_trait::async_trait]
impl Actor for Room {
    type Message = RoomMessage;
    type Context = Context<Self>;

    async fn on_message(&mut self, msg: Self::Message, ctx: &Context<Self>) {
        match msg {
            RoomMessage::JoinRequest(player_id, player) => {
                self.add_player(player_id, player, ctx).await
            }
            RoomMessage::PlayerLeft(player) => self.remove_player(&player).await,
            RoomMessage::PlayerVoted(player_id, vote) => self.set_vote(&player_id, vote).await,
            RoomMessage::ForceOpen => self.force_open().await,
            RoomMessage::Restart => self.restart().await,
            RoomMessage::UpdatePlayer { id, name, voter } => {
                self.update_player(&id, name, voter).await
            }
        }
    }

    async fn setup(&mut self, ctx: &Context<Self>) {
        let players_state: Vec<PlayerState> = self.players.values().map(|p| p.to_state()).collect();
        let game_state = self.to_state();

        self.send_to_players(GamePlayerMessage::Welcome(
            self.id.clone(),
            ctx.addr(),
            game_state.clone(),
            players_state.clone(),
        ))
        .await;
    }
}
