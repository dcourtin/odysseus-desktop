//! HTTP/1.1 réduit à ce que LocalSend utilise : une ligne de requête, des
//! en-têtes, un corps de longueur connue ou découpé (« chunked »).

use std::{
    collections::HashMap,
    io::{self, BufRead, Read, Write},
};

const MAX_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 100;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    /// Noms en minuscules
    headers: Vec<(String, String)>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.get(name).map(String::as_str)
    }

    /// Lecteur du corps de la requête
    pub fn body<'a, R: BufRead>(&self, inner: &'a mut R) -> Body<'a, R> {
        let chunked = self
            .header("transfer-encoding")
            .is_some_and(|t| t.to_ascii_lowercase().contains("chunked"));
        let length = self
            .header("content-length")
            .and_then(|l| l.trim().parse().ok())
            .unwrap_or(0);
        Body {
            inner,
            chunked,
            remaining: if chunked { 0 } else { length },
            done: !chunked && length == 0,
        }
    }
}

fn line<R: BufRead>(r: &mut R) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = r.take(MAX_LINE as u64).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if !buf.ends_with(b"\n") {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "ligne trop longue"));
    }
    Ok(Some(String::from_utf8_lossy(&buf).trim_end().to_string()))
}

/// Requête suivante de la connexion ; None quand le client a fini
pub fn read_request<R: BufRead>(r: &mut R) -> io::Result<Option<Request>> {
    let Some(first) = line(r)? else {
        return Ok(None);
    };
    let mut parts = first.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "requête illisible"));
    };
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let query = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect();
    let mut headers = Vec::new();
    loop {
        let Some(h) = line(r)? else {
            return Err(io::ErrorKind::UnexpectedEof.into());
        };
        if h.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "trop d'en-têtes"));
        }
        if let Some((n, v)) = h.split_once(':') {
            headers.push((n.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    Ok(Some(Request {
        method: method.to_string(),
        path: path.to_string(),
        query,
        headers,
    }))
}

/// Décodage des « %xx » (et « + ») d'un paramètre
fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                        continue;
                    }
                    _ => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct Body<'a, R> {
    inner: &'a mut R,
    chunked: bool,
    /// Octets restants : du corps entier, ou du morceau en cours
    remaining: u64,
    done: bool,
}

impl<R: BufRead> Body<'_, R> {
    /// Corps entier, `limit` octets au plus
    pub fn read_all(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        self.take(limit as u64 + 1).read_to_end(&mut out)?;
        if out.len() > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "corps trop gros"));
        }
        Ok(out)
    }

    /// Lit et jette le reste du corps (pour garder la connexion utilisable)
    pub fn skip(&mut self, limit: u64) -> io::Result<()> {
        let n = io::copy(&mut self.take(limit + 1), &mut io::sink())?;
        if n > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "corps trop gros"));
        }
        Ok(())
    }
}

impl<R: BufRead> Read for Body<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done || buf.is_empty() {
            return Ok(0);
        }
        if self.chunked && self.remaining == 0 {
            let size = line(self.inner)?.ok_or(io::ErrorKind::UnexpectedEof)?;
            let size = size.split(';').next().unwrap_or("").trim();
            let size = u64::from_str_radix(size, 16)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "morceau illisible"))?;
            if size == 0 {
                // En-têtes de fin, jusqu'à la ligne vide
                while line(self.inner)?.is_some_and(|l| !l.is_empty()) {}
                self.done = true;
                return Ok(0);
            }
            self.remaining = size;
        }
        let n = buf.len().min(self.remaining as usize);
        let got = self.inner.read(&mut buf[..n])?;
        if got == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.remaining -= got as u64;
        if self.remaining == 0 {
            if self.chunked {
                // Fin de ligne après chaque morceau
                let mut crlf = [0u8; 2];
                self.inner.read_exact(&mut crlf)?;
            } else {
                self.done = true;
            }
        }
        Ok(got)
    }
}

/// Réponse, avec un corps JSON ou sans corps
pub fn respond(w: &mut impl Write, status: u16, json: Option<&serde_json::Value>) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        _ => "Internal Server Error",
    };
    let body = json.map(|j| j.to_string()).unwrap_or_default();
    let ctype = if json.is_some() {
        "Content-Type: application/json\r\n"
    } else {
        ""
    };
    write!(
        w,
        "HTTP/1.1 {status} {reason}\r\n{ctype}Content-Length: {}\r\n\r\n{body}",
        body.len()
    )?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_chunked_body() {
        let raw = b"POST /api/localsend/v2/upload?sessionId=a%20b&fileId=x HTTP/1.1\r\n\
Transfer-Encoding: chunked\r\nHost: h\r\n\r\n5\r\nhello\r\n6;ext\r\n world\r\n0\r\n\r\n\
GET /next HTTP/1.1\r\nContent-Length: 0\r\n\r\n";
        let mut r = io::BufReader::new(&raw[..]);
        let req = read_request(&mut r).unwrap().unwrap();
        assert_eq!(req.path, "/api/localsend/v2/upload");
        assert_eq!(req.param("sessionId"), Some("a b"));
        assert_eq!(req.header("host"), Some("h"));
        let body = req.body(&mut r).read_all(100).unwrap();
        assert_eq!(body, b"hello world");
        let next = read_request(&mut r).unwrap().unwrap();
        assert_eq!(next.path, "/next");
        assert!(read_request(&mut r).unwrap().is_none());
    }

    #[test]
    fn length_body() {
        let raw = b"POST /x HTTP/1.1\r\nContent-Length: 3\r\n\r\nabcGET";
        let mut r = io::BufReader::new(&raw[..]);
        let req = read_request(&mut r).unwrap().unwrap();
        assert_eq!(req.body(&mut r).read_all(10).unwrap(), b"abc");
    }

    #[test]
    fn percent() {
        assert_eq!(decode("%C3%A9t%C3%A9+x%"), "été x%");
        assert_eq!(decode("a%2"), "a%2");
    }
}
