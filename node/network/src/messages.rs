use crate::broadcast::ProtoContext;
use futures::prelude::*;
use libp2p::request_response::RequestResponseCodec;
use std::io;

pub struct WireMessage {
    pub context: ProtoContext,
    pub payload: Vec<u8>,
    pub is_broadcast: bool,
}

/// Implements the libp2p [`RequestResponseCodec`] trait. Defines how streams of bytes are turned
/// into requests and responses and vice-versa.
#[derive(Debug, Clone)]
#[doc(hidden)] // Needs to be public in order to satisfy the Rust compiler.
pub struct GenericCodec {
    pub max_request_size: u64,
    pub max_response_size: u64,
}

#[async_trait::async_trait]
impl RequestResponseCodec for GenericCodec {
    type Protocol = Vec<u8>;
    type Request = WireMessage;
    type Response = Result<Vec<u8>, ()>;

    async fn read_request<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let is_broadcast = unsigned_varint::aio::read_u8(&mut io)
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        let session_id = unsigned_varint::aio::read_u64(&mut io)
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        let round_index = unsigned_varint::aio::read_u16(&mut io)
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        // Read the length.
        let length = unsigned_varint::aio::read_usize(&mut io)
            .await
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;

        if length > usize::try_from(self.max_request_size).unwrap_or(usize::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Request size exceeds limit: {} > {}",
                    length, self.max_request_size
                ),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;

        Ok(WireMessage {
            context: ProtoContext {
                session_id,
                round_index,
            },
            payload: buffer,
            is_broadcast: is_broadcast != 0,
        })
    }

    async fn read_response<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        // Note that this function returns a `Result<Result<...>>`. Returning an `Err` is
        // considered as a protocol error and will result in the entire connection being closed.
        // Returning `Ok(Err(_))` signifies that a response has successfully been fetched, and
        // that this response is an error.

        // Read the length.
        let length = match unsigned_varint::aio::read_usize(&mut io).await {
            Ok(l) => l,
            Err(unsigned_varint::io::ReadError::Io(err))
                if matches!(err.kind(), io::ErrorKind::UnexpectedEof) =>
            {
                return Ok(Err(()));
            }
            Err(err) => return Err(io::Error::new(io::ErrorKind::InvalidInput, err)),
        };

        if length > usize::try_from(self.max_response_size).unwrap_or(usize::MAX) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Response size exceeds limit: {} > {}",
                    length, self.max_response_size
                ),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;
        Ok(Ok(buffer))
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        // Write broadcast marker
        {
            let mut buffer = unsigned_varint::encode::u8_buffer();
            io.write_all(unsigned_varint::encode::u8(
                req.is_broadcast as u8,
                &mut buffer,
            ))
            .await?;
        }

        // Write session_id
        {
            let mut buffer = unsigned_varint::encode::u64_buffer();
            io.write_all(unsigned_varint::encode::u64(
                req.context.session_id,
                &mut buffer,
            ))
            .await?;
        }

        // Write round_index
        {
            let mut buffer = unsigned_varint::encode::u16_buffer();
            io.write_all(unsigned_varint::encode::u16(
                req.context.round_index,
                &mut buffer,
            ))
            .await?;
        }

        // Write the length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                req.payload.len(),
                &mut buffer,
            ))
            .await?;
        }

        // Write the payload.
        io.write_all(&req.payload).await?;

        io.close().await?;
        Ok(())
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        // If `res` is an `Err`, we jump to closing the substream without writing anything on it.
        if let Ok(res) = res {
            // TODO: check the length?
            // Write the length.
            {
                let mut buffer = unsigned_varint::encode::usize_buffer();
                io.write_all(unsigned_varint::encode::usize(res.len(), &mut buffer))
                    .await?;
            }

            // Write the payload.
            io.write_all(&res).await?;
        }

        io.close().await?;
        Ok(())
    }
}

struct CoordCodec;

#[derive(Clone, Debug, Serialize, Deserialize)]
enum ElectionMessage {
    Propose,
    Aye,
    Nae,
}

#[async_trait::async_trait]
impl RequestResponseCodec for CoordCodec {
    type Protocol = Vec<u8>;
    type Request = ElectionMessage;
    type Response = ElectionMessage;

    async fn read_request<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Request>
    where
        T: AsyncRead + Unpin + Send,
    {
        let length = unsigned_varint::aio::read_usize(&mut io)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        if length > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Request size exceeds limit: {} > {}", length, 1024),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;

        serde_ipld_dagcbor::from_slice(&*buffer)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    async fn read_response<T>(
        &mut self,
        _: &Self::Protocol,
        mut io: &mut T,
    ) -> io::Result<Self::Response>
    where
        T: AsyncRead + Unpin + Send,
    {
        // Read the length.
        let length = unsigned_varint::aio::read_usize(&mut io)
            .await
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        if length > 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Response size exceeds limit: {} > {}", length, 1024),
            ));
        }

        // Read the payload.
        let mut buffer = vec![0; length];
        io.read_exact(&mut buffer).await?;

        serde_ipld_dagcbor::from_slice(&*buffer)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))
    }

    async fn write_request<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        req: Self::Request,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        // Write the length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                req.payload.len(),
                &mut buffer,
            ))
            .await?;
        }

        let bytes = serde_ipld_dagcbor::to_vec(&req)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        // Write the payload.
        io.write_all(&bytes).await?;
        io.close().await?;

        Ok(())
    }

    async fn write_response<T>(
        &mut self,
        _: &Self::Protocol,
        io: &mut T,
        res: Self::Response,
    ) -> io::Result<()>
    where
        T: AsyncWrite + Unpin + Send,
    {
        // Write the length.
        {
            let mut buffer = unsigned_varint::encode::usize_buffer();
            io.write_all(unsigned_varint::encode::usize(
                req.payload.len(),
                &mut buffer,
            ))
            .await?;
        }

        let bytes = serde_ipld_dagcbor::to_vec(&res)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;

        // Write the payload.
        io.write_all(&bytes).await?;
        io.close().await?;

        Ok(())
    }
}
