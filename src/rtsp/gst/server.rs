//! Attempts to subclass RtspServer
//!
//! We are now messing with gstreamer glib objects
//! expect issues

use super::{factory::*, AnyResult};
use anyhow::{anyhow, Context};
use gstreamer::glib::object_subclass;
use gstreamer::glib::subclass::types::ObjectSubclass;
use gstreamer::glib::{self, Object};
use gstreamer_rtsp::RTSPAuthMethod;
use gstreamer_rtsp_server::{
    gio::{TlsAuthenticationMode, TlsCertificate},
    prelude::*,
    subclass::prelude::*,
    RTSPAuth, RTSPServer, RTSPToken, RTSP_TOKEN_MEDIA_FACTORY_ROLE,
};
use log::*;
use neolink_core::bcmedia::model::*;
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    fs,
};
use tokio::sync::{mpsc::Sender, RwLock};

glib::wrapper! {
    /// The wrapped RTSPServer
    pub(crate) struct NeoRtspServer(ObjectSubclass<NeoRtspServerImpl>) @extends RTSPServer;
}

impl Default for NeoRtspServer {
    fn default() -> Self {
        Self::new().unwrap()
    }
}

impl NeoRtspServer {
    pub(crate) fn new() -> AnyResult<Self> {
        gstreamer::init().context("Gstreamer failed to initialise")?;
        let factory = Object::new::<NeoRtspServer>(&[]);
        Ok(factory)
    }

    pub(crate) async fn get_sender<T: Into<String>>(&self, tag: T) -> Option<Sender<BcMedia>> {
        self.imp().get_sender(tag).await
    }

    pub(crate) fn set_credentials(&self, credentials: &[(&str, &str)]) -> AnyResult<()> {
        self.imp().set_credentials(credentials)
    }
    pub(crate) fn set_tls(
        &self,
        cert_file: &str,
        client_auth: TlsAuthenticationMode,
    ) -> AnyResult<()> {
        self.imp().set_tls(cert_file, client_auth)
    }

    pub(crate) async fn create_stream<U: Into<String>>(&self, tag: U) -> AnyResult<()> {
        self.imp().create_stream(tag).await
    }

    #[allow(dead_code)]
    pub(crate) async fn remove_stream<T: Into<String>>(&self, tag: T) -> AnyResult<()> {
        self.imp().remove_stream(tag).await
    }

    #[allow(dead_code)]
    pub(crate) async fn remove_path<T: Into<String>>(
        &self,
        tag: T,
        paths: &[&str],
    ) -> AnyResult<()> {
        self.imp().remove_path(tag, paths).await
    }

    pub(crate) async fn add_path<T: Into<String>>(
        &self,
        tag: T,
        paths: &[String],
    ) -> AnyResult<()> {
        self.imp().add_path(tag, paths).await
    }

    pub(crate) async fn add_permitted_roles<T: Into<String>, U: AsRef<str>>(
        &self,
        tag: T,
        permitted_users: &HashSet<U>,
    ) -> AnyResult<()> {
        self.imp().add_permitted_roles(tag, permitted_users).await
    }

    pub(crate) async fn run(&self, bind_addr: &str, bind_port: u16) -> AnyResult<()> {
        let server = self;
        server.set_address(bind_addr);
        server.set_service(&format!("{}", bind_port));
        // Attach server to default Glib context
        let _ = server.attach(None);
        let main_loop = glib::MainLoop::new(None, false);
        // Run the Glib main loop.
        #[allow(clippy::unit_arg)]
        tokio::task::spawn_blocking(move || Ok(main_loop.run())).await?
    }
}

unsafe impl Send for NeoRtspServer {}
unsafe impl Sync for NeoRtspServer {}

struct FactoryData {
    factory: NeoMediaFactory,
    paths: HashSet<String>,
}

#[derive(Default)]
pub(crate) struct NeoRtspServerImpl {
    medias: RwLock<HashMap<String, FactoryData>>,
}

impl ObjectImpl for NeoRtspServerImpl {}
impl RTSPServerImpl for NeoRtspServerImpl {}

#[object_subclass]
impl ObjectSubclass for NeoRtspServerImpl {
    const NAME: &'static str = "NeoRtspServer";
    type Type = NeoRtspServer;
    type ParentType = RTSPServer;
}

impl NeoRtspServerImpl {
    pub(crate) async fn create_stream<U: Into<String>>(&self, tag: U) -> AnyResult<()> {
        let key = tag.into();
        match self.medias.write().await.entry(key.clone()) {
            Entry::Occupied(_occ) => {}
            Entry::Vacant(vac) => {
                let media = NeoMediaFactory::new();
                vac.insert(FactoryData {
                    factory: media,
                    paths: Default::default(),
                });
            }
        };
        Ok(())
    }

    pub(crate) async fn get_sender<T: Into<String>>(&self, tag: T) -> Option<Sender<BcMedia>> {
        let key = tag.into();
        self.medias
            .read()
            .await
            .get(&key)
            .map(|k| k.factory.get_sender())
    }

    #[allow(dead_code)]
    pub(crate) async fn remove_stream<T: Into<String>>(&self, tag: T) -> AnyResult<()> {
        if let Some(mut media) = self.medias.write().await.remove(&tag.into()) {
            let mounts = self
                .obj()
                .mount_points()
                .expect("The server should have mountpoints");
            for path in media.paths.iter() {
                mounts.remove_factory(path);
            }
            media.paths.clear();
        }
        Ok(())
    }

    pub(crate) async fn add_permitted_roles<T: Into<String>, U: AsRef<str>>(
        &self,
        tag: T,
        permitted_users: &HashSet<U>,
    ) -> AnyResult<()> {
        let tag: String = tag.into();
        if let Some(media) = self.medias.write().await.get_mut(&tag) {
            media.factory.add_permitted_roles(permitted_users);
            Ok(())
        } else {
            Err(anyhow!("No media with tag {} to add users to", &tag))
        }
    }

    pub(crate) async fn add_path<T: Into<String>>(
        &self,
        tag: T,
        paths: &[String],
    ) -> AnyResult<()> {
        let tag = tag.into();
        if let Some(media) = self.medias.write().await.get_mut(&tag) {
            let mounts = self
                .obj()
                .mount_points()
                .expect("The server should have mountpoints");
            for path in paths {
                media.paths.insert(path.clone());
                mounts.add_factory(path, &media.factory);
                // debug!("Adding path: {}", path);
            }
            Ok(())
        } else {
            Err(anyhow!(
                "No media with tag {} to add the paths {:?} to",
                &tag,
                paths
            ))
        }
    }

    pub(crate) async fn remove_path<T: Into<String>>(
        &self,
        tag: T,
        paths: &[&str],
    ) -> AnyResult<()> {
        let tag = tag.into();
        if let Some(media) = self.medias.write().await.get_mut(&tag) {
            let mounts = self
                .obj()
                .mount_points()
                .expect("The server should have mountpoints");
            for path in paths {
                if media.paths.contains(&path.to_string()) {
                    media.paths.remove(&path.to_string());
                    mounts.remove_factory(path);
                }
            }
            Ok(())
        } else {
            Err(anyhow!(
                "No media with tag {} to remove the paths from",
                &tag
            ))
        }
    }

    pub(crate) fn set_credentials(&self, credentials: &[(&str, &str)]) -> AnyResult<()> {
        let auth = self.obj().auth().unwrap_or_else(RTSPAuth::new);
        auth.set_supported_methods(RTSPAuthMethod::Basic);

        let mut un_authtoken = RTSPToken::new(&[(*RTSP_TOKEN_MEDIA_FACTORY_ROLE, &"anonymous")]);
        auth.set_default_token(Some(&mut un_authtoken));

        for credential in credentials {
            let (user, pass) = credential;
            trace!("Setting credentials for user {}", user);
            let token = RTSPToken::new(&[(*RTSP_TOKEN_MEDIA_FACTORY_ROLE, user)]);
            let basic = RTSPAuth::make_basic(user, pass);
            auth.add_basic(basic.as_str(), &token);
        }

        self.obj().set_auth(Some(&auth));
        Ok(())
    }

    pub(crate) fn set_tls(
        &self,
        cert_file: &str,
        client_auth: TlsAuthenticationMode,
    ) -> AnyResult<()> {
        debug!("Setting up TLS using {}", cert_file);
        let auth = self.obj().auth().unwrap_or_else(RTSPAuth::new);

        // We seperate reading the file and changing to a PEM so that we get different error messages.
        let cert_contents = fs::read_to_string(cert_file).with_context(|| "TLS file not found")?;
        let cert = TlsCertificate::from_pem(&cert_contents)
            .with_context(|| "Not a valid TLS certificate")?;
        auth.set_tls_certificate(Some(&cert));
        auth.set_tls_authentication_mode(client_auth);

        self.obj().set_auth(Some(&auth));
        Ok(())
    }
}