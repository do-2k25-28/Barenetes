#![allow(dead_code)]

use proto::cni::v1::{CniRequest, CniResponse};

#[derive(Debug)]
pub struct ProtocolError;

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CNI protocol error")
    }
}

impl std::error::Error for ProtocolError {}

pub fn read_request(_input: impl std::io::Read) -> Result<CniRequest, ProtocolError> {
    todo!("Decode a request from the CNI process input")
}

pub fn write_response(
    _output: impl std::io::Write,
    _response: &CniResponse,
) -> Result<(), ProtocolError> {
    todo!("Encode a response to the CNI process output")
}
