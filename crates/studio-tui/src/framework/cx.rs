//! Messages and the context a component gets with every event.

use std::any::Any;
use std::future::Future;
use std::marker::PhantomData;

use tokio::sync::mpsc::UnboundedSender;
use tokio::task::AbortHandle;

use super::component::ModuleId;
use super::toast::Toasts;

/// A module's own message, boxed for the shared channel.
pub type Payload = Box<dyn Any + Send>;

/// A message on the app's channel, addressed to one module. Modules never
/// build these: [`Cx::send`], [`Cx::spawn`] and [`Sender`] do.
pub struct Msg {
    pub to: ModuleId,
    pub payload: Payload,
}

impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Msg")
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

/// A cloneable, `'static` handle that sends `M` to one module; for tasks that
/// report more than once (progress, a sign-in URL before the result).
pub struct Sender<M> {
    to: ModuleId,
    tx: UnboundedSender<Msg>,
    _m: PhantomData<fn(M)>,
}

impl<M> Clone for Sender<M> {
    fn clone(&self) -> Self {
        Sender {
            to: self.to,
            tx: self.tx.clone(),
            _m: PhantomData,
        }
    }
}

impl<M: Any + Send> Sender<M> {
    /// False when the app has gone away.
    pub fn send(&self, msg: M) -> bool {
        self.tx
            .send(Msg {
                to: self.to,
                payload: Box::new(msg),
            })
            .is_ok()
    }
}

/// What a component can do while handling an event.
pub struct Cx<'a> {
    pub(crate) id: ModuleId,
    pub(crate) tx: &'a UnboundedSender<Msg>,
    pub(crate) toasts: &'a mut Toasts,
    pub(crate) quit: &'a mut bool,
    pub(crate) focused: bool,
}

impl<'a> Cx<'a> {
    /// A context outside the shell, for driving a component directly in a test.
    pub fn detached(
        id: ModuleId,
        tx: &'a UnboundedSender<Msg>,
        toasts: &'a mut Toasts,
        quit: &'a mut bool,
    ) -> Cx<'a> {
        Cx {
            id,
            tx,
            toasts,
            quit,
            focused: true,
        }
    }

    /// The module this context belongs to.
    pub fn id(&self) -> ModuleId {
        self.id
    }

    /// Whether the module is the one on screen.
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// Queues `msg` for this module's `handle_msg`.
    pub fn send<M: Any + Send>(&self, msg: M) {
        let _ = self.tx.send(Msg {
            to: self.id,
            payload: Box::new(msg),
        });
    }

    /// A handle for sending `M` to this module from anywhere.
    pub fn sender<M: Any + Send>(&self) -> Sender<M> {
        Sender {
            to: self.id,
            tx: self.tx.clone(),
            _m: PhantomData,
        }
    }

    /// Runs `fut` on the runtime and delivers its output to this module's
    /// `handle_msg`. Abort it with the returned handle; otherwise let a stale
    /// result be dropped by generation (see [`super::slot::Slot`]).
    pub fn spawn<M, F>(&self, fut: F) -> AbortHandle
    where
        M: Any + Send,
        F: Future<Output = M> + Send + 'static,
    {
        let sender = self.sender::<M>();
        tokio::spawn(async move {
            sender.send(fut.await);
        })
        .abort_handle()
    }

    /// A message on the status line.
    pub fn toast(&mut self, text: impl Into<String>) {
        self.toasts.notify(text);
    }

    /// An error on the status line (red, and it stays longer).
    pub fn error(&mut self, text: impl Into<String>) {
        self.toasts.error(text);
    }

    pub fn toasts(&self) -> &Toasts {
        self.toasts
    }

    /// Ends the app after this event.
    pub fn quit(&mut self) {
        *self.quit = true;
    }
}
