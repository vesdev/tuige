use color_eyre::eyre::{self};
use futures::StreamExt;
use image::DynamicImage;
use lru::LruCache;
use rkyv::{with::CopyOptimize, Archive, Deserialize, Serialize};
use std::{io::Cursor, num::NonZero};
use triomphe::Arc;

pub struct Cache {
    http: reqwest::Client,
    disk_cache_dir: String,
    lru: lru::LruCache<String, Value>,
}

impl Cache {
    pub fn new(disk_cache_dir: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            disk_cache_dir,
            lru: LruCache::new(NonZero::new(100).unwrap()),
        }
    }

    async fn read_cache(
        &mut self,
        use_disk_cache: bool,
        key: &str,
    ) -> eyre::Result<Option<&Value>> {
        if self.lru.contains(key) {
            return Ok(Some(self.lru.get(key).unwrap()));
        }

        if use_disk_cache {
            if let Ok(val) = Self::read_disk_cache_bytes::<Vec<u8>>(&self.disk_cache_dir, key).await
            {
                if let Ok(val) = rkyv::from_bytes::<RawCacheValue>(&val[..]) {
                    self.lru.put(key.into(), val.into());
                    return Ok(Some(self.lru.get(key).unwrap()));
                }
            }
        }

        Ok(None)
    }

    async fn write_cache(
        &mut self,
        use_disk_cache: bool,
        key: &str,
        val: RawCacheValue,
    ) -> eyre::Result<&Value> {
        if use_disk_cache {
            self.write_disk_cache(key, &val).await?;
        }

        self.lru.put(key.into(), val.into());
        Ok(self.lru.get(key).unwrap())
    }

    async fn write_disk_cache(&mut self, key: &str, val: &RawCacheValue) -> eyre::Result<()> {
        let data = rkyv::to_bytes::<RawCacheValue, 1024>(val).unwrap();
        Self::write_disk_cache_bytes(&self.disk_cache_dir, key, data).await?;
        Ok(())
    }

    async fn read_disk_cache_bytes<T: From<Vec<u8>>>(dir: &str, key: &str) -> eyre::Result<T> {
        let value = cacache::read(&dir, key).await?;
        Ok(value.into())
    }

    async fn write_disk_cache_bytes<D: AsRef<[u8]>>(
        dir: &str,
        key: &str,
        data: D,
    ) -> eyre::Result<()> {
        cacache::write(&dir, key, data).await?;
        Ok(())
    }

    async fn cache<
        'a,
        'b,
        Fut: futures::Future<Output = eyre::Result<RawCacheValue>>,
        F: FnOnce(&'a mut Cache) -> Fut,
    >(
        &'a mut self,
        descriptor: CacheValueDescriptor<'b>,
        callback: F,
    ) -> eyre::Result<&'a Value> {
        unsafe {
            if let Ok(Some(v)) = {
                let ptr = self as *mut Self;
                (*ptr)
                    .read_cache(descriptor.use_disk_cache, descriptor.key)
                    .await
            } {
                return Ok(v);
            }

            let ptr = self as *mut Self;
            let value = callback(self);

            (*ptr)
                .write_cache(descriptor.use_disk_cache, descriptor.key, value.await?)
                .await
        }
    }

    pub async fn get_client_id(&mut self, token: &str) -> eyre::Result<&Value> {
        let url = "https://id.twitch.tv/oauth2/validate";

        // Don't store plaintext token in cache
        let hashed_token = blake3::hash(token.as_bytes());

        self.cache(
            CacheValueDescriptor {
                use_disk_cache: true,
                key: &format!("{url}/{hashed_token}"),
            },
            |cache| async move {
                let req = cache.http.get(url).bearer_auth(token).build()?;

                let resp = cache
                    .http
                    .execute(req)
                    .await?
                    .json::<response::twitch::Validate>()
                    .await?;

                Ok(RawCacheValue::ClientId(resp.client_id))
            },
        )
        .await
    }

    pub async fn get_user_id(
        &mut self,
        client_id: &str,
        username: &str,
        token: &str,
    ) -> eyre::Result<&Value> {
        let url = &format!("https://api.twitch.tv/helix/users?login={username}");

        self.cache(
            CacheValueDescriptor {
                use_disk_cache: true,
                key: url,
            },
            |cache| async move {
                let req = cache
                    .http
                    .get(url)
                    .bearer_auth(token)
                    .header("Client-Id", client_id)
                    .build()?;

                let resp = cache
                    .http
                    .execute(req)
                    .await?
                    .json::<response::twitch::User>()
                    .await?;

                Ok(RawCacheValue::UserId(resp.data.first().unwrap().id.clone()))
            },
        )
        .await
    }

    pub async fn get_global_emotes(
        &mut self,
        client_id: String,
        token: String,
    ) -> eyre::Result<&Value> {
        let client_id = Arc::new(client_id);
        let token = Arc::new(token);
        let url = "https://api.twitch.tv/helix/chat/emotes/global";
        self.cache(
            CacheValueDescriptor {
                use_disk_cache: true,
                key: url,
            },
            |cache| async move {
                let req = cache
                    .http
                    .get(url)
                    .bearer_auth(token.as_ref())
                    .header("Client-Id", client_id.as_ref())
                    .build()?;

                let resp = cache
                    .http
                    .execute(req)
                    .await?
                    .json::<response::twitch::GlobalEmotes>()
                    .await?;

                let emote_count = resp.data.len();
                let http = cache.http.clone();
                let set = futures::stream::iter(resp.data)
                    .map(|emote| {
                        let http = http.clone();
                        let token = token.clone();
                        tokio::spawn(async move {
                            let image_1x = emote.images.get("url_1x").unwrap();

                            // download emote
                            let req = http.get(image_1x).bearer_auth(token).build().unwrap();

                            let resp = http.execute(req).await.unwrap().bytes().await.unwrap();

                            Emote::transcode_from_bytes(emote.name, &resp)
                        })
                    })
                    .buffer_unordered(5);

                let set = set
                    .fold(
                        (
                            Vec::<Emote>::with_capacity(emote_count),
                            Vec::<RawEmote>::with_capacity(emote_count),
                        ),
                        |mut s, emote_resp| async move {
                            if let Ok(Ok((emote, image))) = emote_resp {
                                s.1.push(emote);
                                s.0.push(image);
                            }
                            s
                        },
                    )
                    .await;

                Ok(RawCacheValue::EmoteSet(set.1))
            },
        )
        .await
    }
}

struct CacheValueDescriptor<'a> {
    use_disk_cache: bool,
    key: &'a str,
}

#[derive(Debug)]
pub enum Value {
    ClientId(String),
    UserId(String),
    EmoteSet(Vec<Emote>),
}

impl From<RawCacheValue> for Value {
    fn from(value: RawCacheValue) -> Self {
        match value {
            RawCacheValue::ClientId(v) => Value::ClientId(v),
            RawCacheValue::UserId(v) => Value::UserId(v),
            RawCacheValue::EmoteSet(v) => {
                Value::EmoteSet(v.into_iter().map(|e| e.into()).collect())
            }
        }
    }
}

#[derive(Debug)]
pub struct Emote {
    name: String,
    image: DynamicImage,
}

impl From<RawEmote> for Emote {
    fn from(value: RawEmote) -> Self {
        let image =
            image::load_from_memory_with_format(&value.data, image::ImageFormat::WebP).unwrap();

        Self {
            name: value.name,
            image,
        }
    }
}

impl Emote {
    /// Returns both RawEmote and Emote for caching
    fn transcode_from_bytes(name: String, bytes: &[u8]) -> eyre::Result<(RawEmote, Emote)> {
        let image = image::load_from_memory(bytes).unwrap();
        Ok((
            RawEmote::from_image(name.clone(), &image)?,
            Emote { name, image },
        ))
    }
}

#[derive(Archive, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[archive(check_bytes)]
pub enum RawCacheValue {
    ClientId(String),
    UserId(String),
    EmoteSet(Vec<RawEmote>),
}

#[derive(Archive, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[archive(check_bytes)]
pub struct RawEmote {
    name: String,
    #[with(CopyOptimize)]
    data: Vec<u8>,
}

impl RawEmote {
    fn from_image(name: String, image: &image::DynamicImage) -> eyre::Result<Self> {
        let mut data = Vec::new();
        image.write_to(&mut Cursor::new(&mut data), image::ImageFormat::WebP)?;

        Ok(Self { data, name })
    }
}

mod response {

    pub mod twitch {
        use std::collections::HashMap;

        use serde::Deserialize;

        #[derive(Deserialize)]
        pub struct Validate {
            pub client_id: String,
        }

        #[derive(Deserialize)]
        pub struct User {
            pub data: Vec<UserData>,
        }

        #[derive(Deserialize)]
        pub struct UserData {
            pub id: String,
        }

        #[derive(Deserialize)]
        pub struct GlobalEmotes {
            pub data: Vec<GlobalEmoteData>,
        }

        #[derive(Deserialize)]
        pub struct GlobalEmoteData {
            pub name: String,
            pub id: String,
            pub images: HashMap<String, String>,
        }
    }
}
