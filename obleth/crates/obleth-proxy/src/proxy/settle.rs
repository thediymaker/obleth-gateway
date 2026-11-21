//! Settling a request: the figures it is billed with, the term reservations it
//! releases, the stream accounting that settles relayed replies, and the ledger
//! row `finalize` writes.

use super::*;

/// Frozen `(cost_usd, energy)` for a settled request. Energy follows the
/// time the request was served, so a request that held a slot is charged it
/// even when it produced nothing billable; only the token-priced cost is
/// waived then.
fn settled_figures(
    energy: &crate::energy::EnergyEngine,
    billed: bool,
    tokens: (u32, u32),
    rates: (f64, f64, f64),
    request_energy: &crate::energy::RequestEnergy,
    total_ms: u32,
    queue_wait_ms: u32,
) -> (f64, crate::energy::EnergyFigures) {
    let (in_rate, out_rate, modality_cost) = rates;
    let cost_usd = if billed {
        (tokens.0 as f64) * in_rate + (tokens.1 as f64) * out_rate + modality_cost
    } else {
        0.0
    };
    (
        cost_usd,
        energy.figures(request_energy, total_ms, queue_wait_ms),
    )
}

/// A key-scope term reservation made before the tenant step, not yet owned by
/// the settlement guard. If the request ends while it is armed (a tenant
/// rejection that forgot it, or the client leaving mid-await), `Drop` releases
/// it in the background so it cannot block headroom until the TTL lapses.
pub(crate) struct PendingTermHold {
    state: AppState,
    scope: Uuid,
    period: String,
    est: CostEstimate,
    est_cost: f64,
    armed: bool,
}

impl PendingTermHold {
    pub(crate) fn new(
        state: &AppState,
        scope: Uuid,
        period: String,
        est: CostEstimate,
        est_cost: f64,
    ) -> Self {
        Self {
            state: state.clone(),
            scope,
            period,
            est,
            est_cost,
            armed: true,
        }
    }

    /// Release now. The release runs as its own task, so cancelling this
    /// await neither loses it nor lets `Drop` run it a second time.
    pub(crate) async fn release(mut self) {
        self.armed = false;
        let _ = tokio::spawn(self.release_task()).await;
    }

    /// Settlement now owns the reservation.
    pub(crate) fn hand_off(mut self) {
        self.armed = false;
    }

    fn release_task(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let (state, scope, period, est, est_cost) = (
            self.state.clone(),
            self.scope,
            self.period.clone(),
            self.est,
            self.est_cost,
        );
        async move { release_term_hold(&state, &scope, Some(&period), est, est_cost).await }
    }
}

impl Drop for PendingTermHold {
    fn drop(&mut self) {
        if self.armed {
            tokio::spawn(self.release_task());
        }
    }
}

/// Release a term reservation for a request that will never settle (it was
/// rejected by a later admission step). Best effort: a failure only leaves
/// the reservation to lapse with its key's TTL.
async fn release_term_hold(
    state: &AppState,
    scope: &Uuid,
    period: Option<&str>,
    est: CostEstimate,
    est_cost: f64,
) {
    let Some(period) = period else {
        return;
    };
    if let Err(e) = state
        .redis
        .term_reconcile(scope, period, est.total() as i64, est_cost, 0, 0.0)
        .await
    {
        tracing::warn!(error = %e, "term reservation release failed");
    }
}

/// How a drained pass-through response settles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DrainedSettlement {
    pub(super) status: u16,
    pub(super) tokens: UpstreamUsage,
    pub(super) billed: bool,
    /// The body may be stored in the response cache.
    pub(super) cacheable: bool,
}

/// Settlement for a response that has finished streaming to the client.
/// `failure` is the status of a failure seen on the way: the stream broke off
/// (502), or the upstream reported an error inside its own 200. A failed
/// response is billed like a broken-off stream, never with the admission
/// estimate (which is `max_tokens` when the client set it), and is never
/// cached. A clean one settles with its reported usage, or the estimate
/// when the upstream reported none.
pub(super) fn drained_settlement(
    status_code: u16,
    failure: Option<u16>,
    usage: Option<UpstreamUsage>,
    streamed_chars: usize,
    est: CostEstimate,
) -> DrainedSettlement {
    match failure {
        Some(status) => {
            let (tokens, billed) = truncated_stream_billing(usage, streamed_chars, est);
            DrainedSettlement {
                status,
                tokens,
                billed,
                cacheable: false,
            }
        }
        None => DrainedSettlement {
            status: status_code,
            tokens: usage.unwrap_or(UpstreamUsage::uncached(
                est.input_tokens,
                est.estimated_output_tokens,
            )),
            billed: true,
            cacheable: status_code == 200,
        },
    }
}

/// Tokens and billing for a stream that failed: the upstream broke it off, or
/// reported an error in it. Reported usage wins; otherwise the text already
/// delivered is estimated the way the admission estimator counts text
/// (`HeuristicTokenizer::count_text`, ~4 characters per token) on top of the
/// prompt estimate. Nothing streamed means nothing generated, so it is
/// unbilled.
fn truncated_stream_billing(
    usage: Option<UpstreamUsage>,
    streamed_chars: usize,
    est: CostEstimate,
) -> (UpstreamUsage, bool) {
    if let Some(tokens) = usage {
        return (tokens, true);
    }
    if streamed_chars == 0 {
        return (UpstreamUsage::default(), false);
    }
    let output = obleth_tokenizer::output_tokens_from_chars(streamed_chars);
    (UpstreamUsage::uncached(est.input_tokens, output), true)
}

/// Tokens a request settles with: what the upstream reported, or an estimate
/// in its place. `input` is the whole prompt, cached tokens included, so totals
/// and prices keep their meaning; `cached` is the part of it the upstream
/// served from its prefix cache, never more than `input`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct UpstreamUsage {
    pub(crate) input: u32,
    pub(crate) output: u32,
    pub(crate) cached: u32,
}

impl UpstreamUsage {
    pub(crate) fn new(input: u32, output: u32, cached: u32) -> Self {
        Self {
            input,
            output,
            cached: cached.min(input),
        }
    }

    /// Usage with no cached prompt tokens: estimates, cancellations and
    /// answers the gateway composed itself.
    pub(crate) const fn uncached(input: u32, output: u32) -> Self {
        Self {
            input,
            output,
            cached: 0,
        }
    }
}

/// `prompt_tokens_details.cached_tokens` of an OpenAI-style `usage` object,
/// never more than its `prompt_tokens`. Missing or `null` details count as 0.
pub(crate) fn cached_prompt_tokens(usage: &serde_json::Value) -> u64 {
    let prompt = usage
        .get("prompt_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(prompt)
}

/// Usage a buffered chat completion reports, if any.
pub(crate) fn completion_body_usage(body: &serde_json::Value) -> Option<UpstreamUsage> {
    let usage = body.get("usage")?;
    let input = usage.get("prompt_tokens")?.as_u64()? as u32;
    let output = usage
        .get("completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let cached = cached_prompt_tokens(usage) as u32;
    Some(UpstreamUsage::new(input, output, cached))
}

/// Owned admission snapshot: cancellation bookkeeping must not borrow the body
/// that is being dropped. Prices and periods are the same as normal settlement.
#[derive(Clone)]
pub(crate) struct StreamAccounting {
    pub(crate) state: AppState,
    pub(crate) request_id: Uuid,
    pub(crate) resolved: Arc<ResolvedKey>,
    pub(crate) meta: RequestMeta,
    pub(crate) model: String,
    pub(crate) admission: Admission,
    pub(crate) est: CostEstimate,
    pub(crate) queue_wait_ms: u32,
    pub(crate) request_start: Instant,
    pub(crate) cache_status: String,
    pub(crate) capacity: i64,
    pub(crate) term_period: Option<String>,
    pub(crate) key_term_period: Option<String>,
    pub(crate) in_cost_rate: f64,
    pub(crate) out_cost_rate: f64,
    pub(crate) modality_cost: f64,
    pub(crate) energy: crate::energy::RequestEnergy,
    pub(crate) holds: TermHolds,
}

impl StreamAccounting {
    /// Record why the request failed, for its ledger row.
    pub(crate) fn fail(&mut self, class: obleth_config::ErrorClass, reason: &str) {
        self.meta.failure = Some(obleth_config::Failure::new(class, reason));
    }

    /// Record why a drained stream failed, when it settles as a failure: the
    /// upstream's own message when it gave one, else that the stream broke
    /// off (`cut`) or reported a failure without saying what.
    pub(super) fn fail_stream(&mut self, status: u16, cut: bool, message: Option<&str>) {
        if status < 400 {
            return;
        }
        let reason = message.unwrap_or(if cut {
            "the answer stream broke off"
        } else {
            "the model server reported a failure in its answer"
        });
        self.fail(upstream_failure_class(status), reason);
    }

    /// The energy measured for this request up to now.
    pub(super) fn energy_now(&self) -> crate::energy::EnergyFigures {
        self.state.energy.figures(
            &self.energy,
            self.request_start.elapsed().as_millis() as u32,
            self.queue_wait_ms,
        )
    }

    /// Energy accounting is on, so the usage a client receives carries it.
    pub(super) fn energy_on(&self) -> bool {
        self.state.energy.settings().enabled
    }

    /// A rewriter that adds the measured energy to the usage of an answer of
    /// this content type; None while energy accounting is off.
    pub(super) fn energy_stamp(&self, content_type: &str) -> Option<crate::sse::UsageRewriter> {
        crate::sse::UsageRewriter::new(content_type, false, self.energy_on())
    }

    pub(super) fn monitor(
        &self,
        policy: Option<&obleth_config::GuardrailsPolicy>,
        finished: Option<&Result<serde_json::Value, &'static str>>,
    ) {
        if let (Some(policy), Some(finished)) = (policy, finished) {
            match finished {
                Ok(completion) => crate::boons::guardrails::monitor_output(
                    &self.state,
                    &self.state.boons.settings().guardrails,
                    policy,
                    &self.resolved,
                    &self.meta.session_id,
                    self.request_id,
                    completion,
                ),
                Err(reason) => {
                    tracing::warn!(request_id = %self.request_id, reason, "output monitoring skipped")
                }
            }
        }
    }

    /// Cancellation fallback settling as status 499. `tokens` is `None` before
    /// the request has gone to a model server (nothing run, billed nothing),
    /// the prompt alone once it has (the server read it, and with a
    /// non-streaming request may have written the whole answer before its
    /// first byte comes back), and the estimate once the answer has started.
    /// Runtime shutdown remains best-effort, just like the asynchronous
    /// telemetry sink; a partial answer is never cached.
    fn on_cancel(&self, tokens: Option<UpstreamUsage>) -> impl FnOnce() + Send + 'static {
        let mut accounting = self.clone();
        move || {
            let elapsed = accounting.request_start.elapsed().as_millis() as u32;
            accounting.fail(
                obleth_config::ErrorClass::ClientCancel,
                match &tokens {
                    None => "client disconnected before the request reached the model",
                    Some(tokens) if tokens.output == 0 => {
                        "client disconnected before the model answered"
                    }
                    Some(_) => "client disconnected while the answer was being sent",
                },
            );
            let (tokens, billed) = match tokens {
                Some(tokens) => (tokens, true),
                None => (UpstreamUsage::default(), false),
            };
            tokio::spawn(accounting.settle_with(tokens, 0, elapsed, 499, None, billed));
        }
    }

    pub(crate) fn unbilled_guard(&self) -> crate::completion::CompletionGuard {
        crate::completion::CompletionGuard::new(self.on_cancel(None))
    }

    /// From the moment a request goes to a model server, a client that
    /// disconnects is billed its prompt: the server has read it, and closing
    /// the connection just before the answer would otherwise make the work
    /// free. A request the gateway itself fails or times out is still unbilled.
    pub(crate) fn arm_dispatched(&self, guard: &mut crate::completion::CompletionGuard) {
        guard.rearm(self.on_cancel(Some(UpstreamUsage::uncached(self.est.input_tokens, 0))));
    }

    pub(super) fn arm_estimate(&self, guard: &mut crate::completion::CompletionGuard) {
        guard.rearm(self.on_cancel(Some(UpstreamUsage::uncached(
            self.est.input_tokens,
            self.est.estimated_output_tokens,
        ))));
    }

    pub(crate) async fn settle(
        self,
        tokens: UpstreamUsage,
        ttft_ms: u32,
        total_ms: u32,
        status_code: u16,
        cache_put: Option<(String, i64, String, String)>,
    ) {
        self.settle_with(tokens, ttft_ms, total_ms, status_code, cache_put, true)
            .await;
    }

    /// Settle a request that produced nothing billable: zero tokens and
    /// cost, full refund of every reservation (energy is still charged).
    pub(crate) async fn settle_unbilled(self, ttft_ms: u32, total_ms: u32, status_code: u16) {
        self.settle_with(
            UpstreamUsage::default(),
            ttft_ms,
            total_ms,
            status_code,
            None,
            false,
        )
        .await;
    }

    pub(super) async fn settle_with(
        self,
        tokens: UpstreamUsage,
        ttft_ms: u32,
        total_ms: u32,
        status_code: u16,
        cache_put: Option<(String, i64, String, String)>,
        billed: bool,
    ) {
        self.settle_inner(tokens, ttft_ms, total_ms, status_code, cache_put, billed)
            .await;
    }
}

/// End-of-request bookkeeping shared by the streaming pass-through path and
/// the boon interception path: cache store, per-minute budget reconciliation,
/// term-usage commit + budget alerts, and the usage-ledger record.
///
/// `cache_put` carries `(key, ttl, content_type, body)` when the response
/// should be stored for identical future requests; callers gate it on a
/// successful (200) response.
///
/// `holds` names the term reservations made at admission, which are
/// reconciled (released, actual committed) rather than added to. `billed =
/// false` settles a request that produced nothing billable: tokens and cost
/// are frozen at zero (energy is still charged) and the reservations are
/// fully refunded.
impl StreamAccounting {
    async fn settle_inner(
        self,
        tokens: UpstreamUsage,
        ttft_ms: u32,
        total_ms: u32,
        status_code: u16,
        cache_put: Option<(String, i64, String, String)>,
        billed: bool,
    ) {
        let StreamAccounting {
            state,
            request_id,
            resolved,
            meta,
            model,
            admission,
            est,
            queue_wait_ms,
            cache_status,
            capacity,
            term_period,
            key_term_period,
            in_cost_rate,
            out_cost_rate,
            modality_cost,
            energy,
            holds,
            ..
        } = self;
        let (state, resolved, meta, model, cache_status) = (
            &state,
            &*resolved,
            &meta,
            model.as_str(),
            cache_status.as_str(),
        );
        let (term_period, key_term_period) = (term_period.as_deref(), key_term_period.as_deref());
        let request_energy = &energy;
        let cache_put = cache_put.as_ref().map(|(key, ttl, content_type, body)| {
            (key.as_str(), *ttl, content_type.as_str(), body.clone())
        });
        let UpstreamUsage {
            input: input_tokens,
            output: output_tokens,
            cached: cached_input_tokens,
        } = tokens;
        // The crash guard counts a strike against every request that was in
        // flight when the model server's engine died.
        if !meta.crash_fp.is_empty()
            && meta
                .failure
                .as_ref()
                .is_some_and(|f| crate::crash_guard::is_engine_death(&f.reason))
        {
            state
                .crash_guard
                .record_death(&state.redis, model, &meta.crash_fp)
                .await;
        }
        // Feed the router's per-request cost estimate: one EWMA sample of how
        // long this model's answers actually run. Successful requests only — an
        // error body's usage says nothing about the model's answering behavior.
        if status_code == 200 && output_tokens > 0 {
            state.output_stats.observe(model, output_tokens as u64);
        }

        // ---- settlement: the request's cost, then its Redis writes ----
        // Frozen request cost: per-token rates (captured at admission) plus any
        // per-request modality surcharge. Computed once and used for both the
        // term-budget commit and the persisted usage ledger so they agree.
        // Frozen energy figures: the request's share of its model's measured
        // power, or the slot share of node power, over serving time (queue wait
        // excluded). Zeros when accounting is off. Frozen like `cost_usd` so
        // later settings edits never rewrite history.
        let (cost_usd, energy) = settled_figures(
            &state.energy,
            billed,
            (input_tokens, output_tokens),
            (in_cost_rate, out_cost_rate, modality_cost),
            request_energy,
            total_ms,
            queue_wait_ms,
        );
        let added = input_tokens.saturating_add(output_tokens) as i64;
        let est_tokens = est.total() as i64;

        // The response-cache store, the per-minute bucket and the two term
        // commits touch different keys and do not depend on each other, so
        // they go out together instead of as four round trips in a row. Each
        // reports its own failure.
        let store_cached = async {
            // store the full response for identical future requests
            if let Some((ck, ttl, content_type, body)) = cache_put {
                let cached = obleth_config::CachedResponse {
                    status: status_code,
                    content_type: content_type.to_string(),
                    body,
                    input_tokens,
                    output_tokens,
                };
                if let Err(e) = state.redis.cache_put(ck, &cached, ttl).await {
                    tracing::warn!(error = %e, "cache store failed");
                    state.alerts.issue(
                        "redis_cache_store_failed",
                        "Redis response-cache store failed",
                        format!(
                            "tenant `{}` model `{model}` cache ttl `{ttl}`: {e}",
                            resolved.tenant_name
                        ),
                    );
                }
            }
        };
        let reconcile_bucket = async {
            // Reconcile estimate vs actual against the per-minute budget bucket.
            // A zero token rate means the tenant has no per-minute limiter.
            if capacity > 0 {
                if let Err(e) = state
                    .redis
                    .reconcile_budget(
                        &resolved.tenant_id,
                        capacity,
                        est.total(),
                        input_tokens.saturating_add(output_tokens),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "budget reconcile failed");
                    state.alerts.issue(
                        "redis_budget_reconcile_failed",
                        "Redis budget reconcile failed",
                        format!(
                            "tenant `{}` model `{model}` estimated `{}` actual `{}`: {e}",
                            resolved.tenant_name,
                            est.total(),
                            input_tokens.saturating_add(output_tokens),
                        ),
                    );
                }
            }
        };
        let commit_tenant_term = async {
            if let Some(period) = term_period {
                let committed = if holds.tenant {
                    state
                        .redis
                        .term_reconcile(
                            &resolved.tenant_id,
                            period,
                            est_tokens,
                            holds.est_cost,
                            added,
                            cost_usd,
                        )
                        .await
                } else {
                    state
                        .redis
                        .term_usage_add(&resolved.tenant_id, period, added, cost_usd)
                        .await
                };
                match committed {
                    Ok((total_tokens, total_cost)) => {
                        maybe_alert_budget(state, resolved, total_tokens, total_cost);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "term usage commit failed");
                        state.alerts.issue(
                            "redis_term_usage_failed",
                            "Redis term-usage commit failed",
                            format!("tenant `{}` model `{model}`: {e}", resolved.tenant_name),
                        );
                    }
                }
            }
        };
        let commit_key_term = async {
            if let Some(period) = key_term_period {
                let committed = if holds.key {
                    state
                        .redis
                        .term_reconcile(
                            &resolved.key_id,
                            period,
                            est_tokens,
                            holds.est_cost,
                            added,
                            cost_usd,
                        )
                        .await
                } else {
                    state
                        .redis
                        .term_usage_add(&resolved.key_id, period, added, cost_usd)
                        .await
                };
                match committed {
                    Ok((total_tokens, total_cost)) => {
                        maybe_alert_key_budget(state, resolved, total_tokens, total_cost);
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "key term usage commit failed");
                        state.alerts.issue(
                            "redis_key_term_usage_failed",
                            "Redis key term-usage commit failed",
                            format!(
                                "tenant `{}` key `{}` model `{model}`: {e}",
                                resolved.tenant_name, resolved.key_id
                            ),
                        );
                    }
                }
            }
        };
        tokio::join!(
            store_cached,
            reconcile_bucket,
            commit_tenant_term,
            commit_key_term
        );

        finalize(
            state,
            request_id,
            resolved,
            meta,
            model,
            admission,
            est,
            FinalizeOutcome {
                input_tokens,
                output_tokens,
                cached_input_tokens,
                queue_wait_ms,
                ttft_ms,
                total_ms,
                status_code,
                cache_status,
                cost_usd,
                energy,
            },
        );
        state.metrics.total_ms.observe(total_ms as f64);
    }
}

/// The first-token time a request records. A first token is timed only on a
/// reply streamed to the client as it is produced: a buffered reply's first
/// byte arrives with its last, so its "first token" would be its whole
/// duration and skew every latency figure built on it.
fn recorded_ttft(streamed: bool, ttft_ms: u32) -> u32 {
    if streamed {
        ttft_ms
    } else {
        0
    }
}

/// How a request came out, for its ledger row: everything `finalize` records
/// beyond the request's identity. `Default` is a request that produced
/// nothing and cost nothing; a refusal sets its status, time and class.
#[derive(Clone, Debug, Default)]
pub(crate) struct FinalizeOutcome<'a> {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_input_tokens: u32,
    pub queue_wait_ms: u32,
    pub ttft_ms: u32,
    pub total_ms: u32,
    pub status_code: u16,
    /// `off`, `hit`, `miss` or `bypass`; `off` when caching never applied.
    pub cache_status: &'a str,
    pub cost_usd: f64,
    pub energy: crate::energy::EnergyFigures,
}

/// A request refused before it reached an upstream: its status, when it
/// started (the row's `total_ms` is measured here, so no refusal records a
/// zero), what it had reserved, and the cache status it had reached.
pub(crate) struct Rejection<'a> {
    pub status: StatusCode,
    pub since: Instant,
    pub est: CostEstimate,
    pub cache_status: &'a str,
    pub queue_wait_ms: u32,
}

/// Record one rejected request's ledger row. The class and reason come from
/// `meta.failure` and the status, as for every other row.
pub(crate) fn reject(
    state: &AppState,
    request_id: Uuid,
    resolved: &ResolvedKey,
    meta: &RequestMeta,
    model: &str,
    rejection: Rejection<'_>,
) {
    finalize(
        state,
        request_id,
        resolved,
        meta,
        model,
        Admission::Rejected,
        rejection.est,
        FinalizeOutcome {
            total_ms: rejection.since.elapsed().as_millis() as u32,
            status_code: rejection.status.as_u16(),
            cache_status: rejection.cache_status,
            queue_wait_ms: rejection.queue_wait_ms,
            ..FinalizeOutcome::default()
        },
    );
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize(
    state: &AppState,
    request_id: Uuid,
    resolved: &ResolvedKey,
    meta: &RequestMeta,
    model: &str,
    admission: Admission,
    est: CostEstimate,
    outcome: FinalizeOutcome<'_>,
) {
    let FinalizeOutcome {
        input_tokens,
        output_tokens,
        cached_input_tokens,
        queue_wait_ms,
        ttft_ms,
        total_ms,
        status_code,
        cache_status,
        cost_usd,
        energy,
    } = outcome;
    let ttft_ms = recorded_ttft(meta.streamed, ttft_ms);
    state
        .metrics
        .record_request(admission.as_str(), status_code, input_tokens, output_tokens);
    if ttft_ms > 0 {
        state.metrics.ttft_ms.observe(ttft_ms as f64);
    }
    if resolved.internal {
        return;
    }
    let (error_class, error_reason) =
        obleth_config::ledger_failure(status_code, admission.as_str(), meta.failure.as_ref());
    state.telemetry.record(UsageRecord {
        request_id,
        tenant_id: resolved.tenant_id,
        key_id: resolved.key_id,
        model: model.to_string(),
        admission: admission.as_str().to_string(),
        weight: resolved.weight,
        input_tokens,
        output_tokens,
        cached_input_tokens,
        estimated_tokens: est.total(),
        queue_wait_ms,
        ttft_ms,
        total_ms,
        status_code,
        cache_status: cache_status.to_string(),
        cost_usd,
        energy_wh: energy.energy_wh,
        energy_cost_usd: energy.energy_cost_usd,
        co2_g: energy.co2_g,
        ts_ms: crate::tracer::now_ms(),
        session_id: meta.session_id.clone(),
        session_id_source: meta.session_id_source.to_string(),
        request_type: meta.request_type.to_string(),
        device_id: meta.device_id.clone(),
        end_user: meta.end_user.clone(),
        // The client request's own row; helper rows carry its id instead.
        parent_request_id: Uuid::nil(),
        model_variant: meta.model_variant.clone(),
        reasoning_effort: meta.reasoning_effort.clone(),
        reasoning_effort_requested: meta.reasoning_effort_requested.clone(),
        model_family: meta.model_family.clone(),
        key_name: resolved.key_name.clone(),
        tenant_name: resolved.tenant_name.clone(),
        streamed: meta.streamed,
        error_class: error_class.to_string(),
        error_reason,
        client_ip: meta.client.ip_string(),
        client_network: meta.client.network(),
        client_country: meta.client.country.clone(),
        user_agent: meta.client.user_agent.clone(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::lifecycle_tests::{content_event, squash};

    #[test]
    fn only_a_streamed_reply_records_a_first_token() {
        assert!(super::relays_event_stream("text/event-stream"));
        assert!(super::relays_event_stream(
            "text/event-stream; charset=utf-8"
        ));
        assert!(!super::relays_event_stream("application/json"));
        assert_eq!(super::recorded_ttft(true, 420), 420);
        // A buffered reply's first byte is its last: no first token.
        assert_eq!(super::recorded_ttft(false, 34_800), 0);
    }

    #[test]
    fn a_stream_cut_off_after_two_deltas_bills_the_delivered_text() {
        let est = CostEstimate {
            input_tokens: 30,
            estimated_output_tokens: 500,
        };
        // Two content deltas reach the client, then the upstream errors with
        // no usage chunk ever sent.
        let chunks = [content_event("Hello, wor"), content_event("ld and more")];
        let streamed: usize = chunks.iter().map(|c| delta_text_chars(c.as_bytes())).sum();
        assert_eq!(streamed, "Hello, world and more".chars().count());
        let usage = extract_usage(&chunks.concat());
        assert_eq!(usage, None);
        let (tokens, billed) = truncated_stream_billing(usage, streamed, est);
        assert!(billed, "delivered text must be billed");
        assert_eq!(
            tokens,
            UpstreamUsage::uncached(30, 5),
            "prompt estimate + ~4 chars per token"
        );

        // Reported usage wins; nothing streamed is nothing billed.
        let reported = UpstreamUsage::new(7, 3, 4);
        assert_eq!(
            truncated_stream_billing(Some(reported), streamed, est),
            (reported, true)
        );
        assert_eq!(
            truncated_stream_billing(None, 0, est),
            (UpstreamUsage::default(), false)
        );
    }

    #[test]
    fn unbilled_settlement_still_charges_slot_energy() {
        let energy = crate::energy::EnergyEngine::new(obleth_config::EnergySettings {
            enabled: true,
            prometheus_url: "http://prom".into(),
            power_query: "watts".into(),
            model_power_query: String::new(),
            model_power_label: String::new(),
            model_requests_query: String::new(),
            poll_interval_secs: 60,
            energy_cost_per_kwh: 0.10,
            carbon_g_per_kwh: 400.0,
            pue: 1.0,
        });
        energy.store_reading(crate::energy::PowerReading {
            cluster_watts: 409_000.0,
            node_count: 178,
            at_ms: 0,
        });
        // A 503 that produced nothing but held a slot for 2 s.
        let slots = crate::energy::RequestEnergy {
            slots: 8,
            ticket: None,
        };
        let (cost, figures) =
            settled_figures(&energy, false, (0, 0), (0.01, 0.02, 0.5), &slots, 2_000, 0);
        assert_eq!(cost, 0.0, "no billable output, no cost");
        assert!(figures.energy_wh > 0.0, "slot time is still energy");
        let (billed_cost, _) = settled_figures(
            &energy,
            true,
            (100, 50),
            (0.01, 0.02, 0.5),
            &slots,
            2_000,
            0,
        );
        assert!((billed_cost - 2.5).abs() < 1e-9);
    }

    #[test]
    fn completion_body_usage_reads_openai_usage() {
        let body = serde_json::json!({ "usage": { "prompt_tokens": 4, "completion_tokens": 9 } });
        assert_eq!(
            completion_body_usage(&body),
            Some(UpstreamUsage::uncached(4, 9))
        );
        assert_eq!(completion_body_usage(&serde_json::json!({})), None);
    }

    #[test]
    fn completion_body_usage_reads_cached_prompt_tokens() {
        let usage = |details: serde_json::Value| {
            completion_body_usage(&serde_json::json!({ "usage": {
                "prompt_tokens": 100, "completion_tokens": 9, "prompt_tokens_details": details,
            } }))
        };
        assert_eq!(
            usage(serde_json::json!({ "cached_tokens": 64 })),
            Some(UpstreamUsage::new(100, 9, 64))
        );
        // `null` details, details without the count, and a count above the
        // prompt (clamped) all stay within the prompt.
        assert_eq!(
            usage(serde_json::Value::Null),
            Some(UpstreamUsage::uncached(100, 9))
        );
        assert_eq!(
            usage(serde_json::json!({ "audio_tokens": 0 })),
            Some(UpstreamUsage::uncached(100, 9))
        );
        assert_eq!(
            usage(serde_json::json!({ "cached_tokens": 500 })).map(|u| u.cached),
            Some(100)
        );
        // An embedding reports prompt tokens only.
        assert_eq!(
            completion_body_usage(&serde_json::json!({ "usage": {
                "prompt_tokens": 12, "total_tokens": 12,
                "prompt_tokens_details": { "cached_tokens": 8 },
            } })),
            Some(UpstreamUsage::new(12, 0, 8))
        );
    }

    #[test]
    fn a_failed_stream_settles_for_its_output_and_is_never_cached() {
        let est = CostEstimate {
            input_tokens: 30,
            estimated_output_tokens: 8192,
        };
        // An error event after "Hello, world and more" (21 characters)
        // streamed, with no usage reported: the delivered text is billed.
        let failed = drained_settlement(200, Some(500), None, 21, est);
        assert_eq!(
            failed,
            DrainedSettlement {
                status: 500,
                tokens: UpstreamUsage::uncached(30, 5),
                billed: true,
                cacheable: false,
            }
        );
        // An error before any output: nothing generated, nothing billed.
        let empty = drained_settlement(200, Some(400), None, 0, est);
        assert_eq!(
            empty,
            DrainedSettlement {
                status: 400,
                tokens: UpstreamUsage::default(),
                billed: false,
                cacheable: false,
            }
        );
        // Usage the upstream did report (an aborted generation still sends
        // it) wins over the estimate from text.
        let reported = UpstreamUsage::new(12, 7, 4);
        let aborted = drained_settlement(200, Some(502), Some(reported), 99, est);
        assert_eq!((aborted.status, aborted.tokens), (502, reported));
        assert!(aborted.billed && !aborted.cacheable);
        // The admission estimate (here `max_tokens`) is a ceiling, never the
        // output of a failed call.
        for settled in [failed, empty, aborted] {
            assert_ne!(settled.tokens.output, est.estimated_output_tokens);
        }
    }

    #[test]
    fn a_clean_stream_settles_as_it_always_has() {
        let est = CostEstimate {
            input_tokens: 30,
            estimated_output_tokens: 8192,
        };
        let reported = UpstreamUsage::new(12, 7, 4);
        assert_eq!(
            drained_settlement(200, None, Some(reported), 28, est),
            DrainedSettlement {
                status: 200,
                tokens: reported,
                billed: true,
                cacheable: true,
            }
        );
        // With no usage reported the estimate stands in, as before.
        assert_eq!(
            drained_settlement(200, None, None, 28, est).tokens,
            UpstreamUsage::uncached(30, 8192)
        );
        // Only a 200 is ever cached.
        assert!(!drained_settlement(206, None, Some(reported), 0, est).cacheable);
    }

    #[test]
    fn every_reported_failure_is_an_error_status_so_it_never_trains_the_average() {
        // Settlement samples the completion-length average for 200s only.
        let full = include_str!("settle.rs");
        let src = squash(&full[..full.find("\nmod tests {").expect("the test module")]);
        assert!(src.contains(
            "ifstatus_code==200&&output_tokens>0{state.output_stats.observe(model,output_tokens"
        ));
        for payload in [
            serde_json::json!({"error": {"message": "x", "code": 500}}),
            serde_json::json!({"error": {"message": "x", "code": 400}}),
            serde_json::json!({"error": "x"}),
            serde_json::json!({"choices": [{"finish_reason": "abort"}]}),
        ] {
            let failure = crate::upstream_failure::payload_failure(&payload).unwrap();
            assert!((400..=599).contains(&failure.status), "{payload}");
        }
    }

    #[test]
    fn a_dropped_pending_hold_is_released_but_a_handed_off_one_is_not() {
        let full = include_str!("settle.rs");
        let src = squash(&full[..full.find("\nmod tests {").expect("the test module")]);
        let drop_impl = src
            .find("implDropforPendingTermHold{fndrop(&mutself){ifself.armed{tokio::spawn(self.release_task());")
            .is_some();
        assert!(drop_impl, "an armed hold must release itself on drop");
        assert!(src.contains("fnhand_off(mutself){self.armed=false;}"));
    }
}
