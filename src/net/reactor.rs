//! The network thread's event loop: one mio `Poll` for every socket, a waker
//! for the UI's commands and finished lookups, and timeouts taken from each
//! link's next deadline. Nothing here wakes while nothing is due.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use mio::{Events, Poll, Registry, Token, Waker};

use super::link::Link;
use super::{NetCommand, NetEvent, NetworkId, tls};

pub(super) const WAKER: Token = Token(0);

/// What a link needs from the reactor while handling an event.
pub(super) struct Ctx<'a> {
    pub registry: &'a Registry,
    pub tokens: &'a mut Tokens,
    pub sink: &'a mut dyn FnMut(NetEvent),
    pub resolver: &'a Resolver,
    /// `None` if TLS could not be set up; TLS connections then fail.
    pub tls: Option<&'a tls::Shared>,
    pub now: Instant,
}

/// Socket tokens and the network each belongs to.
#[derive(Default)]
pub(super) struct Tokens {
    next: usize,
    owners: HashMap<Token, NetworkId>,
}

impl Tokens {
    pub fn alloc(&mut self, owner: NetworkId) -> Token {
        self.next += 1;
        let token = Token(self.next);
        self.owners.insert(token, owner);
        token
    }

    pub fn free(&mut self, token: Token) {
        self.owners.remove(&token);
    }

    pub fn owner(&self, token: Token) -> Option<NetworkId> {
        self.owners.get(&token).copied()
    }
}

pub(super) struct Resolved {
    pub network: NetworkId,
    pub generation: u64,
    pub result: Result<Vec<SocketAddr>, String>,
}

pub(super) struct Resolver {
    tx: mpsc::Sender<Resolved>,
    waker: Arc<Waker>,
}

impl Resolver {
    /// Looks `host` up on a short-lived thread, since `getaddrinfo` blocks;
    /// the answer comes back through the waker.
    pub fn lookup(&self, network: NetworkId, generation: u64, host: String, port: u16) {
        let (tx, waker) = (self.tx.clone(), self.waker.clone());
        let spawned = std::thread::Builder::new()
            .name(String::from("rubychat-resolve"))
            .spawn(move || {
                let result = (host.as_str(), port)
                    .to_socket_addrs()
                    .map(Iterator::collect)
                    .map_err(|error| error.to_string());
                let _ = tx.send(Resolved {
                    network,
                    generation,
                    result,
                });
                let _ = waker.wake();
            });
        if let Err(error) = spawned {
            let _ = self.tx.send(Resolved {
                network,
                generation,
                result: Err(error.to_string()),
            });
            let _ = self.waker.wake();
        }
    }
}

pub(super) fn run(
    mut poll: Poll,
    waker: Arc<Waker>,
    commands: mpsc::Receiver<NetCommand>,
    mut sink: Box<dyn FnMut(NetEvent) + Send>,
) {
    let tls = tls::shared().ok();
    let (resolved_tx, resolved_rx) = mpsc::channel();
    let resolver = Resolver {
        tx: resolved_tx,
        waker,
    };
    let mut links: BTreeMap<NetworkId, Link> = BTreeMap::new();
    let mut tokens = Tokens::default();
    let mut events = Events::with_capacity(256);
    loop {
        let now = Instant::now();
        let timeout = links
            .values()
            .filter_map(|link| link.next_deadline(now))
            .min()
            .map(|deadline| deadline.saturating_duration_since(now));
        if let Err(error) = poll.poll(&mut events, timeout) {
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        let mut ctx = Ctx {
            registry: poll.registry(),
            tokens: &mut tokens,
            sink: &mut *sink,
            resolver: &resolver,
            tls: tls.as_ref(),
            now: Instant::now(),
        };
        for event in events.iter() {
            let token = event.token();
            if token == WAKER {
                continue;
            }
            if let Some(owner) = ctx.tokens.owner(token)
                && let Some(link) = links.get_mut(&owner)
            {
                let readable = event.is_readable() || event.is_read_closed() || event.is_error();
                link.ready(&mut ctx, token, readable);
            }
        }
        while let Ok(resolved) = resolved_rx.try_recv() {
            if let Some(link) = links.get_mut(&resolved.network) {
                link.resolved(&mut ctx, resolved.generation, resolved.result);
            }
        }
        loop {
            match commands.try_recv() {
                Ok(NetCommand::Connect(id, config)) => {
                    let link = links
                        .entry(id)
                        .or_insert_with(|| Link::new(id, config.clone()));
                    link.set_config(config);
                    link.connect(&mut ctx);
                }
                Ok(NetCommand::Update(id, config)) => {
                    if let Some(link) = links.get_mut(&id) {
                        link.set_config(config);
                    }
                }
                Ok(NetCommand::Send(id, line)) => {
                    if let Some(link) = links.get_mut(&id) {
                        link.send(&mut ctx, line);
                    }
                }
                Ok(NetCommand::Disconnect(id, quit)) => {
                    if let Some(link) = links.get_mut(&id) {
                        link.disconnect(&mut ctx, quit.as_deref());
                    }
                }
                Ok(NetCommand::Close(id)) => {
                    if let Some(mut link) = links.remove(&id) {
                        link.shutdown(&mut ctx);
                    }
                }
                Ok(NetCommand::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => {
                    for link in links.values_mut() {
                        link.shutdown(&mut ctx);
                    }
                    return;
                }
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        let now = ctx.now;
        for link in links.values_mut() {
            if link
                .next_deadline(now)
                .is_some_and(|deadline| deadline <= now)
            {
                link.on_timer(&mut ctx);
            }
        }
    }
}
