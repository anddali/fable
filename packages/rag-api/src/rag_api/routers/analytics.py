"""Dashboard analytics (SSO only).

Admins see platform-wide numbers; regular users see numbers scoped to the
collections they can read and to their own queries.
"""

from __future__ import annotations

import uuid

from fastapi import APIRouter
from sqlalchemy import Integer, cast, func, select, text
from sqlalchemy.orm import selectinload

from rag_api.auth import CurrentUser
from rag_api.deps import DbSession
from rag_core.db.models import (
    ApiKey,
    Chunk,
    Collection,
    Document,
    QueryLog,
    Source,
    SyncRun,
)
from rag_core.rbac import effective_collection_role

router = APIRouter(prefix="/analytics", tags=["analytics"])


async def _readable_ids(session, principal) -> list[uuid.UUID] | None:
    """None = unrestricted (admin)."""
    if principal.is_admin:
        return None
    result = await session.execute(select(Collection).options(selectinload(Collection.principals)))
    return [
        c.id
        for c in result.scalars()
        if effective_collection_role(principal, c.principals, public=c.visibility == "public")
    ]


def _scoped(query, column, ids: list[uuid.UUID] | None):
    return query if ids is None else query.where(column.in_(ids))


@router.get("/db")
async def db_analytics(session: DbSession, principal: CurrentUser) -> dict:
    ids = await _readable_ids(session, principal)

    coll_q = select(Collection.visibility, func.count()).group_by(Collection.visibility)
    if ids is not None:
        coll_q = coll_q.where(Collection.id.in_(ids))
    collections = dict((await session.execute(coll_q)).all())

    docs_q = _scoped(
        select(
            Document.status, func.count(), func.coalesce(func.sum(Document.size_bytes), 0)
        ).group_by(Document.status),
        Document.collection_id,
        ids,
    )
    documents: dict[str, int] = {}
    storage_bytes = 0
    for doc_status, count, size in (await session.execute(docs_q)).all():
        documents[doc_status] = count
        if doc_status != "deleted":
            storage_bytes += int(size)

    chunks = (
        await session.execute(_scoped(select(func.count(Chunk.id)), Chunk.collection_id, ids))
    ).scalar()

    sources_q = _scoped(
        select(Source.connector, func.count()).group_by(Source.connector),
        Source.collection_id,
        ids,
    )
    sources = dict((await session.execute(sources_q)).all())

    per_coll_q = (
        select(
            Collection.id,
            Collection.name,
            Collection.visibility,
            func.count(Document.id).filter(Document.status != "deleted"),
            func.coalesce(func.sum(cast(Document.status == "failed", Integer)), 0),
        )
        .outerjoin(Document, Document.collection_id == Collection.id)
        .group_by(Collection.id, Collection.name, Collection.visibility)
        .order_by(Collection.name)
    )
    if ids is not None:
        per_coll_q = per_coll_q.where(Collection.id.in_(ids))
    per_collection = [
        {
            "id": str(cid),
            "name": name,
            "visibility": visibility,
            "documents": docs,
            "failed": failed,
        }
        for cid, name, visibility, docs, failed in (await session.execute(per_coll_q)).all()
    ]

    runs_q = (
        _scoped(select(SyncRun), SyncRun.collection_id, ids)
        .order_by(SyncRun.started_at.desc())
        .limit(15)
    )
    coll_names = {c["id"]: c["name"] for c in per_collection}
    sync_runs = [
        {
            "collection": coll_names.get(str(r.collection_id), str(r.collection_id)[:8]),
            "trigger": r.trigger,
            "status": r.status,
            "started_at": r.started_at.isoformat(),
            "stats": r.stats,
        }
        for r in (await session.execute(runs_q)).scalars()
    ]

    out = {
        "collections": {
            "total": sum(collections.values()),
            "public": collections.get("public", 0),
            "private": collections.get("private", 0),
        },
        "documents": documents,
        "chunks": chunks,
        "storage_bytes": storage_bytes,
        "sources": sources,
        "per_collection": per_collection,
        "sync_runs": sync_runs,
    }

    if principal.is_admin:
        try:
            rows = (
                await session.execute(
                    text("SELECT status::text, count(*) FROM procrastinate_jobs GROUP BY status")
                )
            ).all()
            out["queue"] = dict(rows)
        except Exception:
            out["queue"] = {}
        out["api_keys"] = {
            "active": (
                await session.execute(
                    select(func.count(ApiKey.id)).where(ApiKey.revoked.is_(False))
                )
            ).scalar(),
            "revoked": (
                await session.execute(select(func.count(ApiKey.id)).where(ApiKey.revoked.is_(True)))
            ).scalar(),
        }
    return out


@router.get("/queries")
async def query_analytics(session: DbSession, principal: CurrentUser, days: int = 14) -> dict:
    days = max(1, min(days, 90))
    window = text(f"now() - interval '{days} days'")

    base = select(QueryLog).where(QueryLog.created_at >= window)
    if not principal.is_admin:
        base = base.where(QueryLog.principal_oid == principal.oid)

    def filtered(q):
        q = q.where(QueryLog.created_at >= window)
        if not principal.is_admin:
            q = q.where(QueryLog.principal_oid == principal.oid)
        return q

    day = func.date_trunc("day", QueryLog.created_at).label("day")
    per_day_rows = (
        await session.execute(
            filtered(select(day, func.count(), func.avg(QueryLog.latency_ms)))
            .group_by(day)
            .order_by(day)
        )
    ).all()
    per_day = [
        {"day": d.date().isoformat(), "queries": int(n), "avg_latency_ms": round(float(lat or 0))}
        for d, n, lat in per_day_rows
    ]

    totals_row = (
        await session.execute(
            filtered(
                select(
                    func.count(),
                    func.coalesce(func.avg(QueryLog.latency_ms), 0),
                    func.coalesce(func.avg(QueryLog.hit_count), 0),
                    func.count(func.distinct(QueryLog.principal_oid)),
                )
            )
        )
    ).one()

    by_method = dict(
        (
            await session.execute(
                filtered(select(QueryLog.auth_method, func.count())).group_by(QueryLog.auth_method)
            )
        ).all()
    )

    top_rows = (
        await session.execute(
            filtered(select(QueryLog.query, func.count().label("n")))
            .group_by(QueryLog.query)
            .order_by(text("n DESC"))
            .limit(10)
        )
    ).all()

    recent_rows = (
        (await session.execute(base.order_by(QueryLog.created_at.desc()).limit(20))).scalars().all()
    )

    return {
        "days": days,
        "totals": {
            "queries": int(totals_row[0]),
            "avg_latency_ms": round(float(totals_row[1])),
            "avg_hits": round(float(totals_row[2]), 1),
            "unique_users": int(totals_row[3]),
        },
        "by_method": by_method,
        "per_day": per_day,
        "top_queries": [{"query": q, "count": int(n)} for q, n in top_rows],
        "recent": [
            {
                "query": r.query,
                "user": r.principal_name or r.principal_oid[:8],
                "auth_method": r.auth_method,
                "mode": r.mode,
                "hits": r.hit_count,
                "latency_ms": r.latency_ms,
                "at": r.created_at.isoformat(),
            }
            for r in recent_rows
        ],
    }
