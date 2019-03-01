use std::thread;
use std::time::Duration;
use std::sync::{Arc, Mutex};
use std::sync::mpsc;

use slog;
use slog_stdlog;
use slog::Drain;

use websocket::client::ClientBuilder;
use serde_json::value::Value;

use receiver::Receiver;
use sender::Sender;
use error::ConnectError;
use message::Message;
use error::{JoinError, MessageError};
use event::EventKind;

type MessageResult = Result<Message, MessageError>;


const PHOENIX_VERSION: &str = "2.0.0";


#[derive(Debug)]
pub enum ClientError {
    Connect(ConnectError),
    Join(JoinError),
    Thread(String),
}

impl From<ConnectError> for ClientError {
    fn from(e: ConnectError) -> Self {
        return ClientError::Connect(e);
    }
}

impl From<JoinError> for ClientError {
    fn from(e: JoinError) -> Self {
        return ClientError::Join(e);
    }
}



pub fn connect(url: &str, params: Vec<(&str, &str)>, logger: Option<slog::Logger>) -> Result<(Sender, Receiver), ConnectError> {
    let logger = logger.unwrap_or(slog::Logger::root(slog_stdlog::StdLog.fuse(), o!()));

    // convert the params to a uri component string
    let mut params_uri: String = "".to_owned();
    for (k, v) in params {
        params_uri.push_str(&format!("&{}={}", k, v));
    }

    // create a phoenix socket url with params expanded and parse it
    // phoenix socket endpoints always have /websocket appended for the socket route
    // it also adds the vsn parameter for versioning
    let addr = format!("{}/websocket?vsn={}{}", url, PHOENIX_VERSION, params_uri);
    let mut client_builder = ClientBuilder::new(&addr)?;

    let socket_client = client_builder.connect_insecure()?;
    let (reader, writer) = socket_client.split()?;

    let sender = Sender::new(writer, logger.new(o!("type" => "sender")));
    let receiver = Receiver::new(reader, logger.new(o!("type" => "receiver")));

    return Ok((sender, receiver));
}


pub struct Client {
    logger: slog::Logger,
    sender_ref: Arc<Mutex<Sender>>,
    heartbeat_handle: thread::JoinHandle<()>,
    message_processor_handle: thread::JoinHandle<()>,
}

impl Client {
    pub fn new(url: &str, params: Vec<(&str, &str)>, logger: Option<slog::Logger>) -> Result<(Client, mpsc::Receiver<MessageResult>), ClientError> {
        let logger = logger.unwrap_or(slog::Logger::root(slog_stdlog::StdLog.fuse(), o!()));
        debug!(logger, "creating client"; "url" => url);

        let (sender, receiver) = connect(url, params, Some(logger.clone()))?;

        let (tx, rx) = mpsc::channel();

        let sender_ref = Arc::new(Mutex::new(sender));
        let heartbeat = Client::keepalive(Arc::clone(&sender_ref));
        let message_processor = Client::process_messages(receiver, tx);

        let client = Client {
            logger: logger,
            sender_ref: sender_ref,
            heartbeat_handle: heartbeat,
            message_processor_handle: message_processor,
        };

        return Ok((client, rx));
    }

    pub fn send(&mut self, topic: &str, event: EventKind, message: &Value) {
        let mut sender = self.sender_ref.lock().unwrap();
        sender.send(topic, event, message);
    }

    fn keepalive(sender_ref: Arc<Mutex<Sender>>) -> thread::JoinHandle<()> {
        return thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(2));
                // if the mutex is poisoned then the whole thread wont work
                let mut sender = sender_ref.lock().unwrap();
                sender.heartbeat();
            }
        });
    }

    fn process_messages(receiver: Receiver, sender: mpsc::Sender<MessageResult>) -> thread::JoinHandle<()> {
        return thread::spawn(move || {
            for message in MessageIterator::new(receiver) {
                let result = sender.send(message);

                // exit the thread cleanly if the channel is closed
                if result.is_err() {
                    break;
                }
            }
        });
    }

    pub fn join(&self, channel: &str) -> Result<u32, ClientError> {
        return match self.sender_ref.lock() {
            Ok(mut sender) => Ok(sender.join(channel)?),
            Err(_) => Err(ClientError::Thread(String::from("Cannot join as sender mutex has been poisoned"))),
        };
    }

    pub fn join_threads(self) -> thread::Result<()> {
        self.heartbeat_handle.join()?;
        self.message_processor_handle.join()?;
        return Ok(());
    }
}


pub struct MessageIterator
{
    receiver: Receiver,
}

impl MessageIterator {
    pub fn new(receiver: Receiver) -> MessageIterator {
        MessageIterator {
            receiver: receiver,
        }
    }
}

impl Iterator for MessageIterator {
    type Item = MessageResult;

    fn next(&mut self) -> Option<Self::Item> {
        return self.receiver.next();
    }
}
