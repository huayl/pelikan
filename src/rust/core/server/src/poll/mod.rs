// Copyright 2021 Twitter, Inc.
// Licensed under the Apache License, Version 2.0
// http://www.apache.org/licenses/LICENSE-2.0

use mio::net::TcpListener;
use mio::Events;
use mio::Interest;
use mio::Token;
use mio::Waker;
use session::Session;
use session::TcpStream;
use slab::Slab;
use std::convert::TryFrom;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
// use mio::Poll;

pub const LISTENER_TOKEN: Token = Token(usize::MAX - 1);
pub const WAKER_TOKEN: Token = Token(usize::MAX);

pub struct Poll {
    listener: Option<TcpListener>,
    poll: mio::Poll,
    sessions: Slab<Session>,
    waker: Arc<Waker>,
}

impl Poll {
    /// Create a new `Poll` instance.
    pub fn new() -> Result<Self, std::io::Error> {
        let poll = mio::Poll::new().map_err(|e| {
            error!("{}", e);
            std::io::Error::new(std::io::ErrorKind::Other, "Failed to create poll instance")
        })?;

        let waker = Arc::new(Waker::new(poll.registry(), WAKER_TOKEN).unwrap());

        let sessions = Slab::<Session>::new();

        Ok(Self {
            listener: None,
            poll,
            sessions,
            waker,
        })
    }

    /// Bind and begin listening on the provided address.
    pub fn bind(&mut self, addr: SocketAddr) -> Result<(), std::io::Error> {
        let mut listener = TcpListener::bind(addr).map_err(|e| {
            error!("{}", e);
            std::io::Error::new(std::io::ErrorKind::Other, "Failed to start tcp listener")
        })?;

        // register listener to event loop
        self.poll
            .registry()
            .register(&mut listener, LISTENER_TOKEN, Interest::READABLE)
            .map_err(|e| {
                error!("{}", e);
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Failed to register listener with epoll",
                )
            })?;

        self.listener = Some(listener);

        Ok(())
    }

    /// Get a copy of the `Waker` for this `Poll` instance
    pub fn waker(&self) -> Arc<Waker> {
        self.waker.clone()
    }

    pub fn poll(&mut self, events: &mut Events, timeout: Duration) -> Result<(), std::io::Error> {
        self.poll.poll(events, Some(timeout))
    }

    pub fn accept(&mut self) -> Result<(TcpStream, SocketAddr), std::io::Error> {
        if let Some(ref listener) = self.listener {
            let (stream, addr) = listener.accept()?;

            // disable Nagle's algorithm
            let _ = stream.set_nodelay(true);

            let stream = TcpStream::try_from(stream)?;
            Ok((stream, addr))
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "not listening",
            ))
        }
    }

    // Session methods

    /// Add a new session
    pub fn add_session(&mut self, mut session: Session) -> Result<Token, std::io::Error> {
        let s = self.sessions.vacant_entry();
        let token = Token(s.key());
        session.set_token(token);
        session.register(&self.poll)?;
        s.insert(session);
        Ok(token)
    }

    /// Close an existing session
    pub fn close_session(&mut self, token: Token) -> Result<(), std::io::Error> {
        trace!("closing session: {}", token.0);
        let mut session = self.remove_session(token)?;
        session.close();
        Ok(())
    }

    /// Remove a session from the poller and return it to the caller
    pub fn remove_session(&mut self, token: Token) -> Result<Session, std::io::Error> {
        let mut session = self.take_session(token)?;
        session.deregister(&self.poll)?;
        Ok(session)
    }

    pub fn get_mut_session(&mut self, token: Token) -> Result<&mut Session, std::io::Error> {
        self.sessions
            .get_mut(token.0)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "no such session"))
    }

    fn take_session(&mut self, token: Token) -> Result<Session, std::io::Error> {
        if self.sessions.contains(token.0) {
            let session = self.sessions.remove(token.0);
            Ok(session)
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "no such session",
            ))
        }
    }

    pub fn reregister(&mut self, token: Token) {
        trace!("reregistering session: {}", token.0);
        if let Some(session) = self.sessions.get_mut(token.0) {
            if session.reregister(&self.poll).is_err() {
                error!("Failed to reregister");
                let _ = self.close_session(token);
            }
        } else {
            trace!("attempted to reregister non-existent session: {}", token.0);
        }
    }
}
