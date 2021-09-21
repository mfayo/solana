use {
    crate::duplicate_shred,
    crossbeam_channel::{RecvTimeoutError, SendError},
    std::{io, sync},
    thiserror::Error,
};

#[derive(Error, Debug)]
pub enum GossipError {
    #[error("duplicate node instance")]
    DuplicateNodeInstance,
    #[error(transparent)]
    DuplicateShredError(#[from] duplicate_shred::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    RecvTimeoutError(#[from] RecvTimeoutError),
    #[error("send error")]
    SendError,
    #[error("serialization error")]
    Serialize(#[from] Box<bincode::ErrorKind>),
}

impl<T> std::convert::From<SendError<T>> for GossipError {
    fn from(_e: SendError<T>) -> GossipError {
        GossipError::SendError
    }
}

impl<T> std::convert::From<sync::mpsc::SendError<T>> for GossipError {
    fn from(_e: sync::mpsc::SendError<T>) -> GossipError {
        GossipError::SendError
    }
}
