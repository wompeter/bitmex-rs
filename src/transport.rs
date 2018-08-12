use std::borrow::Borrow;
use std::collections::BTreeMap;

use chrono::{Duration, Utc};
use failure::Error;
use futures::{Future, Stream};
use hex::encode as hexify;
use hyper::client::{HttpConnector, ResponseFuture};
use hyper::{Body, Client, Method, Request};
use hyper_tls::HttpsConnector;
use ring::{digest, hmac};
use serde::de::DeserializeOwned;
use serde_json::{from_slice, to_string, to_vec};
use url::Url;

use error::{BitMEXError, BitMEXResponse, Result};

#[cfg(feature = "dev")]
const BASE: &'static str = "https://testnet.bitmex.com/api/v1";

#[cfg(not(feature = "dev"))]
const BASE: &'static str = "https://www.bitmex.com/api/v1";

const EXPIRE_DURATION: i64 = 5;

pub(crate) type Dummy = &'static [(&'static str, &'static str); 0];

pub struct Transport {
    client: Client<HttpsConnector<HttpConnector>>,
    credential: Option<(String, String)>,
}

impl Transport {
    pub fn new() -> Self {
        let https = HttpsConnector::new(4).unwrap();
        let client = Client::builder().build::<_, Body>(https);

        Transport { client: client, credential: None }
    }

    pub fn with_credential(api_key: &str, api_secret: &str) -> Self {
        let https = HttpsConnector::new(4).unwrap();
        let client = Client::builder().build::<_, Body>(https);

        Transport {
            client: client,
            credential: Some((api_key.into(), api_secret.into())),
        }
    }

    pub fn get<O: DeserializeOwned, I, K, V>(&self, endpoint: &str, params: Option<I>) -> Result<impl Future<Item = O, Error = Error>>
    where
        I: IntoIterator,
        I::Item: Borrow<(K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.request::<_, _, Dummy, _, _, _, _>(Method::GET, endpoint, params, None)
    }

    pub fn signed_get<O: DeserializeOwned, I, K, V>(&self, endpoint: &str, params: Option<I>) -> Result<impl Future<Item = O, Error = Error>>
    where
        I: IntoIterator,
        I::Item: Borrow<(K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.signed_request::<_, _, Dummy, _, _, _, _>(Method::GET, endpoint, params, None)
    }

    pub fn signed_post<O: DeserializeOwned, I, K, V>(&self, endpoint: &str, data: Option<I>) -> Result<impl Future<Item = O, Error = Error>>
    where
        I: IntoIterator,
        I::Item: Borrow<(K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.signed_request::<_, Dummy, _, _, _, _, _>(Method::POST, endpoint, None, data)
    }

    pub fn signed_put<O: DeserializeOwned, I, K, V>(&self, endpoint: &str, params: Option<I>) -> Result<impl Future<Item = O, Error = Error>>
    where
        I: IntoIterator,
        I::Item: Borrow<(K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.signed_request::<_, _, Dummy, _, _, _, _>(Method::PUT, endpoint, params, None)
    }

    pub fn signed_delete<O: DeserializeOwned, I, K, V>(&self, endpoint: &str, params: Option<I>) -> Result<impl Future<Item = O, Error = Error>>
    where
        I: IntoIterator,
        I::Item: Borrow<(K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        self.signed_request::<_, _, Dummy, _, _, _, _>(Method::DELETE, endpoint, params, None)
    }

    pub fn request<O: DeserializeOwned, I, J, K1, V1, K2, V2>(
        &self,
        method: Method,
        endpoint: &str,
        params: Option<I>,
        data: Option<J>,
    ) -> Result<impl Future<Item = O, Error = Error>>
    where
        I: IntoIterator,
        I::Item: Borrow<(K1, V1)>,
        K1: AsRef<str>,
        V1: AsRef<str>,
        J: IntoIterator,
        J::Item: Borrow<(K2, V2)>,
        K2: AsRef<str>,
        V2: AsRef<str>,
    {
        let url = format!("{}/{}", BASE, endpoint);
        let url = match params {
            Some(p) => Url::parse_with_params(&url, p)?,
            None => Url::parse(&url)?,
        };

        let body = match data {
            Some(data) => {
                let bt = data
                    .into_iter()
                    .map(|i| {
                        let (a, b) = i.borrow();
                        (a.as_ref().to_string(), b.as_ref().to_string())
                    })
                    .collect::<BTreeMap<_, _>>();
                Body::from(to_vec(&bt)?)
            }
            None => Body::empty(),
        };

        let req = Request::builder().method(method).uri(url.as_str()).header("content-type", "application/json").body(body)?;
        Ok(self.handle_response(self.client.request(req)))
    }

    pub fn signed_request<O: DeserializeOwned, I, J, K1, V1, K2, V2>(
        &self,
        method: Method,
        endpoint: &str,
        params: Option<I>,
        data: Option<J>,
    ) -> Result<impl Future<Item = O, Error = Error>>
    where
        I: IntoIterator,
        I::Item: Borrow<(K1, V1)>,
        K1: AsRef<str>,
        V1: AsRef<str>,
        J: IntoIterator,
        J::Item: Borrow<(K2, V2)>,
        K2: AsRef<str>,
        V2: AsRef<str>,
    {
        let url = format!("{}/{}", BASE, endpoint);
        let url = match params {
            Some(p) => Url::parse_with_params(&url, p)?,
            None => Url::parse(&url)?,
        };

        let body = match data {
            Some(data) => {
                let bt = data
                    .into_iter()
                    .map(|i| {
                        let (a, b) = i.borrow();
                        (a.as_ref().to_string(), b.as_ref().to_string())
                    })
                    .collect::<BTreeMap<_, _>>();
                to_string(&bt)?
            }
            None => "".to_string(),
        };

        let expires = (Utc::now() + Duration::seconds(EXPIRE_DURATION)).timestamp();
        let (key, signature) = self.signature(Method::GET, expires, &url, &body)?;

        let req = Request::builder()
            .method(method)
            .uri(url.as_str())
            .header("api-expires", expires)
            .header("api-key", key)
            .header("api-signature", signature)
            .header("content-type", "application/json")
            .body(Body::from(body))?;

        Ok(self.handle_response(self.client.request(req)))
    }

    fn check_key(&self) -> Result<(&str, &str)> {
        match self.credential.as_ref() {
            None => Err(BitMEXError::NoApiKeySet)?,
            Some((k, s)) => Ok((k, s)),
        }
    }

    pub(self) fn signature(&self, method: Method, expires: i64, url: &Url, body: &str) -> Result<(&str, String)> {
        let (key, secret) = self.check_key()?;
        // Signature: hex(HMAC_SHA256(apiSecret, verb + path + expires + data))
        let signed_key = hmac::SigningKey::new(&digest::SHA256, secret.as_bytes());
        let sign_message = match url.query() {
            Some(query) => format!("{}{}?{}{}{}", method.as_str(), url.path(), query, expires, body),
            None => format!("{}{}{}{}", method.as_str(), url.path(), expires, body),
        };
        let signature = hexify(hmac::sign(&signed_key, sign_message.as_bytes()));
        Ok((key, signature))
    }

    fn handle_response<O: DeserializeOwned>(&self, fut: ResponseFuture) -> impl Future<Item = O, Error = Error> {
        fut.from_err::<Error>()
            .and_then(|resp| resp.into_body().concat2().from_err::<Error>())
            .map(|chunk| {
                trace!("{}", String::from_utf8_lossy(&*chunk));
                chunk
            })
            .and_then(|chunk| Ok(from_slice(&chunk)?))
            .and_then(|resp: BitMEXResponse<O>| Ok(resp.to_result()?))
    }
}

#[cfg(test)]
mod test {
    use super::Transport;
    use error::Result;
    use hyper::Method;
    use url::Url;

    #[test]
    fn test_signature_get() -> Result<()> {
        let tr = Transport::with_credential("LAqUlngMIQkIUjXMUreyu3qn", "chNOOS4KvNXR_Xq4k4c9qsfoKWvnDecLATCRlcBwyKDYnWgO");
        let (_, sig) = tr.signature(Method::GET, 1518064236, &Url::parse("http://a.com/api/v1/instrument")?, "")?;
        assert_eq!(sig, "c7682d435d0cfe87c16098df34ef2eb5a549d4c5a3c2b1f0f77b8af73423bf00");
        Ok(())
    }

    #[test]
    fn test_signature_get_param() -> Result<()> {
        let tr = Transport::with_credential("LAqUlngMIQkIUjXMUreyu3qn", "chNOOS4KvNXR_Xq4k4c9qsfoKWvnDecLATCRlcBwyKDYnWgO");
        let (_, sig) = tr.signature(
            Method::GET,
            1518064237,
            &Url::parse_with_params("http://a.com/api/v1/instrument", &[("filter", r#"{"symbol": "XBTM15"}"#)])?,
            "",
        )?;
        assert_eq!(sig, "e2f422547eecb5b3cb29ade2127e21b858b235b386bfa45e1c1756eb3383919f");
        Ok(())
    }

    #[test]
    fn test_signature_post() -> Result<()> {
        let tr = Transport::with_credential("LAqUlngMIQkIUjXMUreyu3qn", "chNOOS4KvNXR_Xq4k4c9qsfoKWvnDecLATCRlcBwyKDYnWgO");
        let (_, sig) = tr.signature(
            Method::POST,
            1518064238,
            &Url::parse("http://a.com/api/v1/order")?,
            r#"{"symbol":"XBTM15","price":219.0,"clOrdID":"mm_bitmex_1a/oemUeQ4CAJZgP3fjHsA","orderQty":98}"#,
        )?;
        assert_eq!(sig, "1749cd2ccae4aa49048ae09f0b95110cee706e0944e6a14ad0b3a8cb45bd336b");
        Ok(())
    }
}
