//! Closed dispatch façade for the process-isolated parity helper.

use tracedecay_sqlite_parity_protocol::{
    Command, ErrorCode, ErrorPayload, Output, PROTOCOL_VERSION, Request, Response, ResponseOutcome,
    decode_request_value, validate_command,
};

use crate::{snapshot, snapshot::ReadOnlyDriver, transport::MAX_REQUEST_BYTES};

pub(crate) fn handle_request_bytes(bytes: &[u8]) -> Response {
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return error_response(
            None,
            ErrorPayload::new(
                ErrorCode::RequestTooLarge,
                format!("request exceeds {MAX_REQUEST_BYTES} bytes"),
            ),
        );
    }
    let value: serde_json::Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(error) => {
            return error_response(
                None,
                ErrorPayload::new(ErrorCode::InvalidRequest, format!("invalid JSON: {error}")),
            );
        }
    };
    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned);
    let request: Request = match decode_request_value(value) {
        Ok(request) => request,
        Err(error) => return error_response(request_id, error),
    };

    let verified_snapshot = match snapshot::verify_copied_snapshot(&request.database) {
        Ok(snapshot) => snapshot,
        Err(error) => return error_response(Some(request.request_id), error),
    };
    let outcome = ReadOnlyDriver::open(&verified_snapshot)
        .and_then(|driver| {
            let output = execute(&driver, request.command)?;
            snapshot::validate_verified_snapshot(&verified_snapshot)?;
            Ok(output)
        })
        .map_or_else(
            |error| ResponseOutcome::Error { error },
            |output| ResponseOutcome::Ok { output },
        );
    Response {
        protocol_version: PROTOCOL_VERSION,
        request_id: Some(request.request_id),
        verified_snapshot: Some(verified_snapshot),
        outcome,
    }
}

fn execute(driver: &ReadOnlyDriver, command: Command) -> Result<Output, ErrorPayload> {
    validate_command(&command)?;
    match command {
        Command::Metadata => driver.metadata().map(Output::Metadata),
        Command::Schema => driver.schema().map(Output::Schema),
        Command::ForeignKeys => driver.foreign_keys(),
        Command::PageSize => driver.page_size(),
        Command::JournalMode => driver.journal_mode().map(Output::JournalMode),
        Command::Integrity { check } => driver.integrity(check).map(Output::Integrity),
        Command::SessionStoreCount { family, table } => driver
            .session_store_count(family, table)
            .map(Output::SessionStoreCount),
        Command::SessionStoreSchema { family, table } => driver
            .session_store_schema(family, table)
            .map(Output::SessionStoreSchema),
        Command::SessionStorePage {
            family,
            table,
            cursor,
            limit,
        } => driver
            .session_store_page(family, table, cursor, limit)
            .map(Output::SessionStorePage),
    }
}

fn error_response(request_id: Option<String>, error: ErrorPayload) -> Response {
    Response {
        protocol_version: PROTOCOL_VERSION,
        request_id,
        verified_snapshot: None,
        outcome: ResponseOutcome::Error { error },
    }
}
