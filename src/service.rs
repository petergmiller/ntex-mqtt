use std::task::{Context, Poll};
use std::{fmt, future::Future, marker::PhantomData, pin::Pin, rc::Rc};

use ntex::codec::{Decoder, Encoder};
use ntex::io::{DispatchItem, Filter, Io, IoBoxed};
use ntex::service::{Service, ServiceFactory};
use ntex::time::{Seconds, Sleep};
use ntex::util::{select, Either};

use crate::io::Dispatcher;

type ResponseItem<U> = Option<<U as Encoder>::Item>;

pub struct MqttServer<St, C, T, Codec> {
    connect: C,
    handler: Rc<T>,
    disconnect_timeout: Seconds,
    _t: PhantomData<(St, Codec)>,
}

impl<St, C, T, Codec> MqttServer<St, C, T, Codec> {
    pub(crate) fn new(connect: C, service: T, disconnect_timeout: Seconds) -> Self {
        MqttServer { connect, disconnect_timeout, handler: Rc::new(service), _t: PhantomData }
    }
}

impl<St, C, T, Codec> MqttServer<St, C, T, Codec>
where
    C: ServiceFactory<IoBoxed, Response = (IoBoxed, Codec, St, Seconds)>,
{
    fn create_service(
        &self,
    ) -> impl Future<Output = Result<MqttHandler<St, C::Service, T, Codec>, C::InitError>> {
        let fut = self.connect.new_service(());
        let handler = self.handler.clone();
        let disconnect_timeout = self.disconnect_timeout;

        // create connect service and then create service impl
        async move {
            Ok(MqttHandler {
                handler,
                disconnect_timeout,
                connect: fut.await?,
                _t: PhantomData,
            })
        }
    }
}

impl<St, C, T, Codec> ServiceFactory<IoBoxed> for MqttServer<St, C, T, Codec>
where
    St: 'static,
    C: ServiceFactory<IoBoxed, Response = (IoBoxed, Codec, St, Seconds)> + 'static,
    C::Error: fmt::Debug,
    T: ServiceFactory<
            DispatchItem<Codec>,
            St,
            Response = ResponseItem<Codec>,
            Error = C::Error,
            InitError = C::Error,
        > + 'static,
    Codec: Decoder + Encoder + Clone + 'static,
{
    type Response = ();
    type Error = C::Error;
    type InitError = C::InitError;
    type Service = MqttHandler<St, C::Service, T, Codec>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Service, Self::InitError>>>>;

    fn new_service(&self, _: ()) -> Self::Future {
        Box::pin(self.create_service())
    }
}

impl<F, St, C, T, Codec> ServiceFactory<Io<F>> for MqttServer<St, C, T, Codec>
where
    F: Filter,
    St: 'static,
    C: ServiceFactory<IoBoxed, Response = (IoBoxed, Codec, St, Seconds)> + 'static,
    C::Error: fmt::Debug,
    T: ServiceFactory<
            DispatchItem<Codec>,
            St,
            Response = ResponseItem<Codec>,
            Error = C::Error,
            InitError = C::Error,
        > + 'static,
    Codec: Decoder + Encoder + Clone + 'static,
{
    type Response = ();
    type Error = C::Error;
    type InitError = C::InitError;
    type Service = MqttHandler<St, C::Service, T, Codec>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Service, Self::InitError>>>>;

    fn new_service(&self, _: ()) -> Self::Future {
        Box::pin(self.create_service())
    }
}

impl<St, C, T, Codec> ServiceFactory<(IoBoxed, Option<Sleep>)> for MqttServer<St, C, T, Codec>
where
    St: 'static,
    C: ServiceFactory<IoBoxed, Response = (IoBoxed, Codec, St, Seconds)> + 'static,
    C::Error: fmt::Debug,
    T: ServiceFactory<
            DispatchItem<Codec>,
            St,
            Response = ResponseItem<Codec>,
            Error = C::Error,
            InitError = C::Error,
        > + 'static,
    Codec: Decoder + Encoder + Clone + 'static,
{
    type Response = ();
    type Error = C::Error;
    type InitError = C::InitError;
    type Service = MqttHandler<St, C::Service, T, Codec>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Service, Self::InitError>>>>;

    fn new_service(&self, _: ()) -> Self::Future {
        Box::pin(self.create_service())
    }
}

pub struct MqttHandler<St, C, T, Codec> {
    connect: C,
    handler: Rc<T>,
    disconnect_timeout: Seconds,
    _t: PhantomData<(St, Codec)>,
}

impl<St, C, T, Codec> Service<IoBoxed> for MqttHandler<St, C, T, Codec>
where
    St: 'static,
    C: Service<IoBoxed, Response = (IoBoxed, Codec, St, Seconds)> + 'static,
    C::Error: fmt::Debug,
    T: ServiceFactory<
            DispatchItem<Codec>,
            St,
            Response = ResponseItem<Codec>,
            Error = C::Error,
            InitError = C::Error,
        > + 'static,
    Codec: Decoder + Encoder + Clone + 'static,
{
    type Response = ();
    type Error = C::Error;
    type Future = Pin<Box<dyn Future<Output = Result<(), Self::Error>>>>;

    #[inline]
    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.connect.poll_ready(cx)
    }

    #[inline]
    fn poll_shutdown(&self, cx: &mut Context<'_>, is_error: bool) -> Poll<()> {
        self.connect.poll_shutdown(cx, is_error)
    }

    #[inline]
    fn call(&self, req: IoBoxed) -> Self::Future {
        let handler = self.handler.clone();
        let timeout = self.disconnect_timeout;
        let handshake = self.connect.call(req);

        Box::pin(async move {
            let (io, codec, session, keepalive) = handshake.await.map_err(|e| {
                log::trace!("Connection handshake failed: {:?}", e);
                e
            })?;
            log::trace!("Connection handshake succeeded");

            let handler = handler.new_service(session).await?;
            log::trace!("Connection handler is created, starting dispatcher");

            Dispatcher::new(io, codec, handler)
                .keepalive_timeout(keepalive)
                .disconnect_timeout(timeout)
                .await
        })
    }
}

impl<F, St, C, T, Codec> Service<Io<F>> for MqttHandler<St, C, T, Codec>
where
    F: Filter,
    St: 'static,
    C: Service<IoBoxed, Response = (IoBoxed, Codec, St, Seconds)> + 'static,
    C::Error: fmt::Debug,
    T: ServiceFactory<
            DispatchItem<Codec>,
            St,
            Response = ResponseItem<Codec>,
            Error = C::Error,
            InitError = C::Error,
        > + 'static,
    Codec: Decoder + Encoder + Clone + 'static,
{
    type Response = ();
    type Error = C::Error;
    type Future = Pin<Box<dyn Future<Output = Result<(), Self::Error>>>>;

    #[inline]
    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.connect.poll_ready(cx)
    }

    #[inline]
    fn poll_shutdown(&self, cx: &mut Context<'_>, is_error: bool) -> Poll<()> {
        self.connect.poll_shutdown(cx, is_error)
    }

    #[inline]
    fn call(&self, io: Io<F>) -> Self::Future {
        Service::<IoBoxed>::call(self, IoBoxed::from(io))
    }
}

impl<St, C, T, Codec> Service<(IoBoxed, Option<Sleep>)> for MqttHandler<St, C, T, Codec>
where
    St: 'static,
    C: Service<IoBoxed, Response = (IoBoxed, Codec, St, Seconds)> + 'static,
    C::Error: fmt::Debug,
    T: ServiceFactory<
            DispatchItem<Codec>,
            St,
            Response = ResponseItem<Codec>,
            Error = C::Error,
            InitError = C::Error,
        > + 'static,
    Codec: Decoder + Encoder + Clone + 'static,
{
    type Response = ();
    type Error = C::Error;
    type Future = Pin<Box<dyn Future<Output = Result<(), Self::Error>>>>;

    #[inline]
    fn poll_ready(&self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.connect.poll_ready(cx)
    }

    #[inline]
    fn poll_shutdown(&self, cx: &mut Context<'_>, is_error: bool) -> Poll<()> {
        self.connect.poll_shutdown(cx, is_error)
    }

    #[inline]
    fn call(&self, (io, delay): (IoBoxed, Option<Sleep>)) -> Self::Future {
        let handler = self.handler.clone();
        let timeout = self.disconnect_timeout;
        let handshake = self.connect.call(io);

        Box::pin(async move {
            let (io, codec, ka, handler) = if let Some(delay) = delay {
                let res = select(
                    delay,
                    Box::pin(async {
                        let (io, codec, st, ka) = handshake.await.map_err(|e| {
                            log::trace!("Connection handshake failed: {:?}", e);
                            e
                        })?;
                        log::trace!("Connection handshake succeeded");

                        let handler = handler.new_service(st).await?;
                        log::trace!("Connection handler is created, starting dispatcher");

                        Ok::<_, C::Error>((io, codec, ka, handler))
                    }),
                )
                .await;

                match res {
                    Either::Left(_) => {
                        log::warn!("Handshake timed out");
                        return Ok(());
                    }
                    Either::Right(item) => item?,
                }
            } else {
                let (io, codec, st, ka) = handshake.await.map_err(|e| {
                    log::trace!("Connection handshake failed: {:?}", e);
                    e
                })?;
                log::trace!("Connection handshake succeeded");

                let handler = handler.new_service(st).await?;
                log::trace!("Connection handler is created, starting dispatcher");
                (io, codec, ka, handler)
            };

            Dispatcher::new(io, codec, handler)
                .keepalive_timeout(ka)
                .disconnect_timeout(timeout)
                .await
        })
    }
}
