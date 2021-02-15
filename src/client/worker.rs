//! Handlers for all incoming data
use super::observer::OBSERVER;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

use super::{
    client::{Client, ClientState},
    tdlib_client::{RawApi, TdLibClient},
};
use crate::types::Update;
use crate::{
    errors::RTDError,
    errors::RTDResult,
    tdjson::ClientId,
    types::{
        from_json, AuthorizationState, AuthorizationStateWaitCode,
        AuthorizationStateWaitEncryptionKey, AuthorizationStateWaitOtherDeviceConfirmation,
        AuthorizationStateWaitPassword, AuthorizationStateWaitPhoneNumber,
        AuthorizationStateWaitRegistration, CheckAuthenticationCode, CheckAuthenticationPassword,
        CheckDatabaseEncryptionKey, GetApplicationConfig, RObject, RegisterUser,
        SetAuthenticationPhoneNumber, SetTdlibParameters, TdType, UpdateAuthorizationState,
    },
};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::{
    sync::{mpsc, RwLock},
    task::JoinHandle,
    time,
};

/// `AuthStateHandler` trait provides methods that returns data, required for authentication
///It allows you to handle particular "auth states", such as [WaitPassword](crate::types::AuthorizationStateWaitPassword), [WaitPhoneNumber](crate::types::AuthorizationStateWaitPhoneNumber) and so on.
#[async_trait]
pub trait AuthStateHandler {
    /// Interacts with provided link
    async fn handle_other_device_confirmation(
        &self,
        wait_device_confirmation: &AuthorizationStateWaitOtherDeviceConfirmation,
    );
    /// Returns wait code
    async fn handle_wait_code(&self, wait_code: &AuthorizationStateWaitCode) -> String;
    /// Returns database encryption key
    async fn handle_encryption_key(
        &self,
        wait_encryption_key: &AuthorizationStateWaitEncryptionKey,
    ) -> String;
    /// Returns password
    async fn handle_wait_password(&self, wait_password: &AuthorizationStateWaitPassword) -> String;
    /// Returns phone number
    async fn handle_wait_phone_number(
        &self,
        wait_phone_number: &AuthorizationStateWaitPhoneNumber,
    ) -> String;
    /// Returns first_name and second_name
    async fn handle_wait_registration(
        &self,
        wait_registration: &AuthorizationStateWaitRegistration,
    ) -> (String, String);
}

/// Provides minimal implementation of `AuthStateHandler`.
/// All required methods wait for stdin input
#[derive(Debug, Clone)]
pub struct ConsoleAuthStateHandler;

impl Default for ConsoleAuthStateHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsoleAuthStateHandler {
    pub fn new() -> Self {
        Self
    }

    fn wait_input() -> String {
        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(_) => input.trim().to_string(),
            Err(e) => panic!("Can not get input value: {:?}", e),
        }
    }
}

#[async_trait]
impl AuthStateHandler for ConsoleAuthStateHandler {
    async fn handle_other_device_confirmation(
        &self,
        wait_device_confirmation: &AuthorizationStateWaitOtherDeviceConfirmation,
    ) {
        eprintln!(
            "other device confirmation link: {}",
            wait_device_confirmation.link()
        );
    }

    async fn handle_wait_code(&self, _wait_code: &AuthorizationStateWaitCode) -> String {
        eprintln!("wait for auth code");
        ConsoleAuthStateHandler::wait_input()
    }

    async fn handle_encryption_key(
        &self,
        _wait_encryption_key: &AuthorizationStateWaitEncryptionKey,
    ) -> String {
        eprintln!("wait for encryption key");
        ConsoleAuthStateHandler::wait_input()
    }

    async fn handle_wait_password(
        &self,
        _wait_password: &AuthorizationStateWaitPassword,
    ) -> String {
        eprintln!("wait for password");
        ConsoleAuthStateHandler::wait_input()
    }

    async fn handle_wait_phone_number(
        &self,
        _wait_phone_number: &AuthorizationStateWaitPhoneNumber,
    ) -> String {
        eprintln!("wait for phone number");
        ConsoleAuthStateHandler::wait_input()
    }

    async fn handle_wait_registration(
        &self,
        _wait_registration: &AuthorizationStateWaitRegistration,
    ) -> (String, String) {
        loop {
            eprintln!("waits for first_name and second_name separated by comma");
            let input: String = ConsoleAuthStateHandler::wait_input();
            let found: Vec<&str> = input.splitn(2, |c| c == ',').collect();
            if let 2 = found.len() {
                let f = found.get(0).unwrap().trim();
                let s = found.get(1).unwrap().trim();
                if !f.is_empty() && !s.is_empty() {
                    return (f.to_string(), s.to_string());
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct WorkerBuilder<A, T>
where
    A: AuthStateHandler + Send + Sync + 'static,
    T: TdLibClient + Send + Sync + Clone + 'static,
{
    read_updates_timeout: f64,
    channels_send_timeout: f64,
    auth_state_handler: A,
    tdlib_client: T,
}

impl Default for WorkerBuilder<ConsoleAuthStateHandler, RawApi> {
    /// Provides default implementation with [ConsoleAuthStateHandler](crate::client::client::ConsoleAuthStateHandler)
    fn default() -> Self {
        Self {
            read_updates_timeout: 2.0,
            channels_send_timeout: 5.0,
            auth_state_handler: ConsoleAuthStateHandler::new(),
            tdlib_client: RawApi::new(),
        }
    }
}

impl<A, T> WorkerBuilder<A, T>
where
    A: AuthStateHandler + Send + Sync + 'static,
    T: TdLibClient + Send + Sync + Clone + 'static,
{
    pub fn with_channels_send_timeout(mut self, timeout: f64) -> Self {
        self.channels_send_timeout = timeout;
        self
    }

    pub fn with_read_updates_timeout(mut self, read_updates_timeout: f64) -> Self {
        self.read_updates_timeout = read_updates_timeout;
        self
    }

    /// [AuthStateHandler](crate::client::client::AuthStateHandler) allows you to handle particular "auth states", such as [WaitPassword](crate::types::AuthorizationStateWaitPassword), [WaitPhoneNumber](crate::types::AuthorizationStateWaitPhoneNumber) and so on.
    /// See [AuthorizationState](crate::types::AuthorizationState).
    pub fn with_auth_state_handler<N>(self, auth_state_handler: N) -> WorkerBuilder<N, T>
    where
        N: AuthStateHandler + Send + Sync + 'static,
    {
        WorkerBuilder {
            auth_state_handler,
            read_updates_timeout: self.read_updates_timeout,
            channels_send_timeout: self.channels_send_timeout,
            tdlib_client: self.tdlib_client,
        }
    }

    #[doc(hidden)]
    pub fn with_tdlib_client<C>(self, tdlib_client: C) -> WorkerBuilder<A, C>
    where
        C: TdLibClient + Send + Sync + Clone + 'static,
    {
        WorkerBuilder {
            tdlib_client,
            auth_state_handler: self.auth_state_handler,
            read_updates_timeout: self.read_updates_timeout,
            channels_send_timeout: self.channels_send_timeout,
        }
    }

    pub fn build(self) -> RTDResult<Worker<A, T>> {
        let worker = Worker::new(
            self.auth_state_handler,
            self.read_updates_timeout,
            self.channels_send_timeout,
            self.tdlib_client,
        );
        Ok(worker)
    }
}

/// A high-level abstraction of TDLib.
/// Before start any API interactions you must call `start().await`.
#[derive(Debug, Clone)]
pub struct Worker<A, S>
where
    A: AuthStateHandler + Send + Sync + 'static,
    S: TdLibClient + Send + Sync + Clone + 'static ,
{
    stop_flag: Arc<AtomicBool>,
    auth_state_handler: Arc<A>,
    read_updates_timeout: f64,
    channels_send_timeout: f64,
    tdlib_client: S,
    clients: Arc<RwLock<HashMap<ClientId, (Client<S>, mpsc::Sender<ClientState>)>>>,
}

impl Worker<ConsoleAuthStateHandler, RawApi> {
    pub fn builder() -> WorkerBuilder<ConsoleAuthStateHandler, RawApi> {
        WorkerBuilder::default()
    }
}

impl<A, T> Worker<A, T>
where
    A: AuthStateHandler + Send + Sync + 'static,
    T: TdLibClient + Send + Sync + Clone + 'static,
{
    pub async fn auth_client(
        &mut self,
        mut client: Client<T>,
    ) -> RTDResult<(JoinHandle<ClientState>, Client<T>)> {
        let client_id = self.tdlib_client.new_client();
        log::debug!("new client created: {}", client_id);
        client.set_client_id(client_id)?;
        let (sx, mut rx) = mpsc::channel::<ClientState>(10);
        self.clients
            .write()
            .await
            .insert(client_id, (client.clone(), sx));
        log::debug!("new client added");

        client
            .get_application_config(GetApplicationConfig::builder().build())
            .await?;

        log::trace!("received first internal response");

        if let Some(msg) = rx.recv().await {
            match msg {
                ClientState::Closed => {
                    debug!("client state on auth: closed");
                    return Ok((tokio::spawn(async { ClientState::Closed }), client));
                }
                ClientState::Error(e) => {
                    debug!("client state on auth: error");
                    return Err(RTDError::TdlibError(e));
                }
                ClientState::Opened => {
                    debug!("client state on auth: opened");
                }
            }
        }
        let h = tokio::spawn(async move {
            match rx.recv().await {
                Some(ClientState::Opened) => {
                    ClientState::Error("received Opened state again".to_string())
                }
                Some(state) => state,
                None => ClientState::Error("auth state channel closed".to_string()),
            }
        });
        Ok((h, client))
    }

    #[cfg(test)]
    // Method needs for tests because we can't handle get_application_config request properly.
    pub async fn set_client(
        &mut self,
        mut client: Client<T>,
    ) -> Client<T> {
        let (sx, mut rx) = mpsc::channel::<ClientState>(10);
        let cl = self.tdlib_client.new_client();
        self.clients
            .write()
            .await
            .insert(cl, (client.clone(), sx));
        client.set_client_id(cl).unwrap();
        let h = tokio::spawn(async {
            ClientState::Opened
        });
        client
    }

    // Client must be created only with builder
    pub(crate) fn new(
        auth_state_handler: A,
        read_updates_timeout: f64,
        channels_send_timeout: f64,
        tdlib_client: T,
    ) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let clients: HashMap<i32, (Client<T>, mpsc::Sender<ClientState>)> = HashMap::new();

        Self {
            stop_flag,
            read_updates_timeout,
            tdlib_client,
            channels_send_timeout,
            auth_state_handler: Arc::new(auth_state_handler),
            clients: Arc::new(RwLock::new(clients)),
        }
    }

    /// Starts interaction with TDLib.
    /// It returns [JoinHandle](tokio::task::JoinHandle) which allows you to handle worker state.
    pub fn start(&mut self) -> JoinHandle<ClientState> {
        let (auth_sx, auth_rx) = mpsc::channel::<UpdateAuthorizationState>(20);

        let updates_handle = self.init_updates_task(auth_sx);
        let auth_handle = self.init_auth_task(auth_rx);

        let stop = self.stop_flag.clone();

        tokio::spawn(async move {
            let res_state = tokio::select! {
                a = auth_handle => match a {
                    Ok(_) => {
                        ClientState::Closed
                    },
                    Err(e) => ClientState::Error(e.to_string()),
                },
                u = updates_handle => match u {
                    Ok(_) => {
                        ClientState::Closed
                    },
                    Err(e) => ClientState::Error(e.to_string()),
                },
            };
            stop.store(true, Ordering::Release);
            res_state
        })
    }

    /// Stops the client.
    /// You may want to await JoinHandle retrieved with `client.start().await` after stopping the client.
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Release)
    }

    // pub(crate) is just for unit-tests
    // It's the base routine: sends received updates to particular handlers: observer and auth_state handler
    pub(crate) fn init_updates_task(
        &self,
        auth_sx: mpsc::Sender<UpdateAuthorizationState>,
    ) -> JoinHandle<RTDResult<()>> {
        let stop_flag = self.stop_flag.clone();
        let clients = self.clients.clone();
        let recv_timeout = self.read_updates_timeout;
        let send_timeout = time::Duration::from_secs_f64(self.channels_send_timeout);
        let tdlib_client = Arc::new(self.tdlib_client.clone());
        tokio::spawn(async move {
            let current = tokio::runtime::Handle::try_current().unwrap();
            while !stop_flag.load(Ordering::Acquire) {
                let cl = tdlib_client.clone();
                if let Some(json) = current
                    .spawn_blocking(move || cl.receive(recv_timeout))
                    .await
                    .unwrap()
                {
                    trace!("received json from tdlib: {}", json);
                    match from_json::<TdType>(&json) {
                        Ok(t) => match OBSERVER.notify(t) {
                            None => {}
                            Some(t) => {
                                if let TdType::Update(update) = t {
                                    if let Update::AuthorizationState(auth_state) = update {
                                        trace!("auth state send: {:?}", auth_state);
                                        auth_sx.send_timeout(auth_state, send_timeout).await?;
                                        trace!("auth state sent");
                                    } else {
                                        if let Some(client_id) = update.client_id() {
                                            match clients.read().await.get(&client_id) {
                                                None => {
                                                    warn!(
                                                        "found updates for unavailable client ({})",
                                                        client_id
                                                    )
                                                }
                                                Some((client, _)) => {
                                                    if let Some(sender) = client.updates_sender() {
                                                        trace!("sending update to client");
                                                        sender
                                                            .send_timeout(update, send_timeout)
                                                            .await?;
                                                        trace!("update sent");
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        },
                        Err(e) => {
                            panic!("{}", e)
                        }
                    };
                }
            }
            Ok(())
        })
    }

    // created task handles [UpdateAuthorizationState][crate::types::UpdateAuthorizationState] and sends it to particular methods of specified [AuthStateHandler](crate::client::client::AuthStateHandler)
    fn init_auth_task(
        &self,
        mut auth_rx: mpsc::Receiver<UpdateAuthorizationState>,
    ) -> JoinHandle<RTDResult<()>> {
        let auth_state_handler = self.auth_state_handler.clone();
        let clients = self.clients.clone();
        let send_timeout = time::Duration::from_secs_f64(self.channels_send_timeout);

        tokio::spawn(async move {
            while let Some(auth_state) = auth_rx.recv().await {
                debug!("received new auth state: {:?}", auth_state);
                if let Some(client_id) = auth_state.client_id() {
                    match clients.read().await.get(&client_id) {
                        None => {
                            warn!("found auth updates for unavailable client ({})", client_id)
                        }
                        Some((client, auth_sender)) => {
                            handle_auth_state(
                                client,
                                auth_sender,
                                auth_state_handler.as_ref(),
                                auth_state,
                                send_timeout,
                            )
                            .await?;
                            debug!("state handled properly")
                        }
                    }
                }
            }
            Ok(())
        })
    }
}

async fn handle_auth_state<A: AuthStateHandler, R: TdLibClient + Clone>(
    client: &Client<R>,
    auth_sender: &mpsc::Sender<ClientState>,
    auth_state_handler: &A,
    state: UpdateAuthorizationState,
    send_state_timeout: time::Duration,
) -> RTDResult<()> {
    debug!("handling new auth state: {:?}", state);
    match state.authorization_state() {
        AuthorizationState::_Default(_) => Ok(()),
        AuthorizationState::Closed(_) => {
            auth_sender
                .send_timeout(ClientState::Closed, send_state_timeout)
                .await?;
            Ok(())
        }
        AuthorizationState::Closing(_) => Ok(()),
        AuthorizationState::LoggingOut(_) => Ok(()),
        AuthorizationState::Ready(_) => {
            debug!("ready state received, send signal");
            auth_sender
                .send_timeout(ClientState::Opened, send_state_timeout)
                .await?;
            Ok(())
        }
        AuthorizationState::WaitCode(wait_code) => {
            let code = auth_state_handler.handle_wait_code(wait_code).await;
            client
                .check_authentication_code(CheckAuthenticationCode::builder().code(code).build())
                .await?;
            Ok(())
        }
        AuthorizationState::WaitEncryptionKey(wait_encryption_key) => {
            let key = auth_state_handler
                .handle_encryption_key(wait_encryption_key)
                .await;
            debug!("checking encryption key");
            client
                .check_database_encryption_key(
                    CheckDatabaseEncryptionKey::builder()
                        .encryption_key(key)
                        .build(),
                )
                .await?;
            debug!("encryption key check done");
            Ok(())
        }
        AuthorizationState::WaitOtherDeviceConfirmation(wait_device_confirmation) => {
            debug!("handling other device confirmation");
            auth_state_handler
                .handle_other_device_confirmation(wait_device_confirmation)
                .await;
            debug!("handled other device confirmation");
            Ok(())
        }
        AuthorizationState::WaitPassword(wait_password) => {
            let password = auth_state_handler.handle_wait_password(wait_password).await;
            debug!("checking password");
            client
                .check_authentication_password(
                    CheckAuthenticationPassword::builder()
                        .password(password)
                        .build(),
                )
                .await?;
            debug!("password checked");
            Ok(())
        }
        AuthorizationState::WaitPhoneNumber(wait_phone_number) => {
            let phone_number = auth_state_handler
                .handle_wait_phone_number(wait_phone_number)
                .await;
            client
                .set_authentication_phone_number(
                    SetAuthenticationPhoneNumber::builder()
                        .phone_number(phone_number)
                        .build(),
                )
                .await?;
            Ok(())
        }
        AuthorizationState::WaitRegistration(wait_registration) => {
            debug!("handling wait registration");
            let (first_name, last_name) = auth_state_handler
                .handle_wait_registration(wait_registration)
                .await;
            let register = RegisterUser::builder()
                .first_name(first_name)
                .last_name(last_name)
                .build();
            client.register_user(register).await?;
            debug!("handled register user");
            Ok(())
        }
        AuthorizationState::WaitTdlibParameters(_) => {
            debug!("going to set tdlib parameters");
            client
                .set_tdlib_parameters(
                    SetTdlibParameters::builder()
                        .parameters(client.tdlib_parameters())
                        .build(),
                )
                .await?;
            debug!("tdlib parameters set");
            Ok(())
        }
        AuthorizationState::GetAuthorizationState(_) => Err(RTDError::Internal(
            "retrieved GetAuthorizationState update but observer not found any subscriber",
        )),
    }
}

// #[cfg(test)]
// mod tests {
//     use crate::client::{AuthStateHandler, Client};
//     use crate::client::client::ClientBuilder;
//     use crate::types::*;
//     use async_trait::async_trait;
//
//     struct DummyStateHandler;
//     #[async_trait]
//     impl AuthStateHandler for DummyStateHandler {
//         async fn handle_other_device_confirmation(
//             &self,
//             _: &AuthorizationStateWaitOtherDeviceConfirmation,
//         ) {
//             unimplemented!()
//         }
//         async fn handle_wait_code(&self, _: &AuthorizationStateWaitCode) -> String {
//             unimplemented!()
//         }
//
//         async fn handle_encryption_key(&self, _: &AuthorizationStateWaitEncryptionKey) -> String {
//             unimplemented!()
//         }
//         async fn handle_wait_password(&self, _: &AuthorizationStateWaitPassword) -> String {
//             unimplemented!()
//         }
//         async fn handle_wait_phone_number(&self, _: &AuthorizationStateWaitPhoneNumber) -> String {
//             unimplemented!()
//         }
//         async fn handle_wait_registration(
//             &self,
//             _: &AuthorizationStateWaitRegistration,
//         ) -> (String, String) {
//             unimplemented!()
//         }
//     }
//
//     #[test]
//     fn test_builder_auth_state_handler() {
//         Client::builder()
//             .with_tdlib_parameters(TdlibParameters::builder().build())
//             .build()
//             .unwrap();
//         ClientBuilder::default()
//             .with_tdlib_parameters(TdlibParameters::builder().build())
//             .build()
//             .unwrap();
//         ClientBuilder::default()
//             .with_tdlib_parameters(TdlibParameters::builder().build())
//             .build()
//             .unwrap();
//     }
//
//     #[test]
//     fn test_builder_no_params() {
//         let result = Client::builder().build();
//
//         if result.is_ok() {
//             panic!("client wrongly build without tdlib params")
//         }
//
//         Client::builder()
//             .with_tdlib_parameters(TdlibParameters::builder().build())
//             .build()
//             .unwrap();
//     }
// }
