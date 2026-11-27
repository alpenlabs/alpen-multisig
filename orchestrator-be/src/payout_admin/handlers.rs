use super::*;
use auth::Auth;
use axum::{
    extract::{DefaultBodyLimit, Path, State},
    routing::{get, post},
    Json, Router,
};

/// Mounts the challenge, session verification, and transaction routes on the shared service.
pub fn router(service: Arc<Service>) -> Router {
    Router::new()
        .route(CHALLENGE, post(auth::challenge))
        .route(VERIFY, post(auth::verify))
        .route(PENDING, get(pending).post(submit))
        .route(
            REPORT,
            // JSON escapes one byte as at most 6 bytes; 1 KiB covers the wrapper object.
            post(attach_report).layer(DefaultBodyLimit::max(6 * MAX_REPORT_BYTES + 1024)),
        )
        .route(SIGNATURES, post(paste))
        .route(PAST, get(past))
        .route(TRANSACTIONS, get(copy))
        .route(BROADCAST, post(broadcast))
        .with_state(service)
}

async fn pending(
    _: Auth,
    State(service): State<Arc<Service>>,
) -> Result<Json<Vec<PendingBlockPayoutsView>>> {
    Ok(Json(service.list_pending().await?))
}

async fn submit(
    _: Auth,
    State(service): State<Arc<Service>>,
    Json(request): Json<PsbtRequest>,
) -> Result<Json<PendingBlockPayoutsView>> {
    Ok(Json(
        service
            .submit(&request.psbt, request.report, SystemTime::now())
            .await?,
    ))
}

async fn attach_report(
    _: Auth,
    State(service): State<Arc<Service>>,
    Path(id): Path<String>,
    Json(request): Json<ReportRequest>,
) -> Result<Json<()>> {
    service
        .attach_report(&id, request.report, SystemTime::now())
        .await?;

    Ok(Json(()))
}

async fn paste(
    _: Auth,
    State(service): State<Arc<Service>>,
    Path(id): Path<String>,
    Json(request): Json<SignaturesRequest>,
) -> Result<Json<PasteSignaturesView>> {
    Ok(Json(
        service
            .paste(&id, &request.signatures, SystemTime::now())
            .await?,
    ))
}

async fn past(
    _: Auth,
    State(service): State<Arc<Service>>,
) -> Result<Json<Vec<PastBlockPayoutsView>>> {
    Ok(Json(
        service
            .database
            .list()
            .await?
            .into_iter()
            .filter_map(|record| record.past)
            .collect(),
    ))
}

async fn copy(
    _: Auth,
    State(service): State<Arc<Service>>,
    Path(id): Path<String>,
) -> Result<Json<TransactionCopy>> {
    let record = service.get(&id).await?;

    Ok(Json(match record.past {
        Some(past) => TransactionCopy::RawTx(past.raw_tx),
        None if SystemTime::now() < record.expires_at => TransactionCopy::Psbt(record.psbt),
        None => return Err(AppError::NotFound),
    }))
}

async fn broadcast(
    _: Auth,
    State(service): State<Arc<Service>>,
    Path(id): Path<String>,
) -> Result<Json<BroadcastResponse>> {
    let record = service.get(&id).await?;

    if let Some(past) = record.past {
        let transaction = bitcoin::consensus::deserialize(&hex::decode(past.raw_tx).map_err(bad)?)
            .map_err(bad)?;
        service.blockchain.broadcast(transaction).await?;
    } else {
        service
            .refresh_inner(record, SystemTime::now(), &mut HashMap::new())
            .await?;
    }

    let record = service.get(&id).await?;

    Ok(Json(BroadcastResponse {
        txid: id.parse().map_err(bad)?,
        broadcast_error: if record.past.is_some() {
            None
        } else {
            record.broadcast_error
        },
    }))
}
