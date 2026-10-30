const DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS = 32000;
const GATEWAY_OUTPUT_RESERVE_TOKENS = 1024;
const MIN_THINKING_BUDGET_TOKENS = 1024;
const EFFORT_RANKS = { none: 0, low: 1, medium: 2, high: 3, xhigh: 4, max: 5 };

let catalog = null;
let allModels = [];
let selectedKey = null;

const activeFilters = new Set();

const els = {
  meta: document.getElementById("catalog-meta"),
  search: document.getElementById("search-input"),
  providerSelect: document.getElementById("provider-select"),
  sortSelect: document.getElementById("sort-select"),
  resultsCount: document.getElementById("results-count"),
  providerBanner: document.getElementById("provider-banner"),
  tbody: document.getElementById("models-tbody"),
  inspectorEmpty: document.getElementById("inspector-empty"),
  inspectorContent: document.getElementById("inspector-content"),
};

function fmtNum(n) {
  if (n === null || n === undefined) return "—";
  if (n >= 1000000 && n % 1000000 === 0) return `${n / 1000000}M`;
  if (n >= 1000) return `${(n / 1000).toFixed(n % 1000 === 0 ? 0 : 1)}k`;
  return String(n);
}

function fmtCost(c) {
  if (!c || (c.input === null && c.output === null)) return "—";
  const inp = c.input !== null && c.input !== undefined ? `$${c.input}` : "—";
  const out = c.output !== null && c.output !== undefined ? `$${c.output}` : "—";
  return `${inp} / ${out}`;
}

function isChatModel(model) {
  if (!model.modalities) return true;
  const inp = model.modalities.input || [];
  const out = model.modalities.output || [];
  return inp.includes("text") && out.includes("text");
}

function normalizeEffort(model, requested) {
  if (!model.supports_reasoning && (!model.reasoning_options || model.reasoning_options.length === 0)) {
    return null;
  }
  const supported =
    model.reasoning_options && model.reasoning_options.length > 0
      ? model.reasoning_options
      : ["low", "medium", "high"];
  if (supported.includes(requested)) return requested;
  const target = EFFORT_RANKS[requested] ?? 2;
  let best = supported[0];
  let bestDist = Infinity;
  for (const opt of supported) {
    const rank = EFFORT_RANKS[opt] ?? 0;
    const dist = Math.abs(rank - target);
    if (dist < bestDist || (dist === bestDist && rank < (EFFORT_RANKS[best] ?? 0))) {
      best = opt;
      bestDist = dist;
    }
  }
  return best;
}

function resolveGatewayMaxOutputTokens(model, estimatedInput, requestedMax, reasoningBudget) {
  const reasoningFloor =
    reasoningBudget && reasoningBudget > 0 ? reasoningBudget + GATEWAY_OUTPUT_RESERVE_TOKENS : 0;
  const baseCap =
    requestedMax && requestedMax > 0
      ? Math.max(requestedMax, reasoningFloor)
      : Math.max(DEFAULT_GATEWAY_MAX_OUTPUT_TOKENS, reasoningFloor);
  let resolved = Math.min(baseCap, model.max_tokens);
  if (model.context_window && model.context_window > 0) {
    const used = estimatedInput + GATEWAY_OUTPUT_RESERVE_TOKENS;
    if (used >= model.context_window) {
      return {
        error: `ContextOverflowError: estimated_input (${estimatedInput}) + reserve (${GATEWAY_OUTPUT_RESERVE_TOKENS}) >= context_window (${model.context_window})`,
      };
    }
    resolved = Math.min(resolved, model.context_window - used);
  }
  return { ok: resolved };
}

function toWireReasoningParams(protocol, model, requestedEffort, maxOutputTokens) {
  const normalized = normalizeEffort(model, requestedEffort);
  if (!normalized) return { kind: "disabled" };

  const idLower = model.id.toLowerCase();
  const famLower = (model.family || "").toLowerCase();
  const isToggle =
    idLower.includes("glm-") ||
    idLower.includes("/glm") ||
    famLower.includes("glm") ||
    idLower.includes("deepseek");

  if (isToggle) {
    return { kind: "thinking_toggle", enabled: normalized !== "none" };
  }
  if (normalized === "none") {
    return { kind: "disabled" };
  }

  const available = Math.max(0, maxOutputTokens - GATEWAY_OUTPUT_RESERVE_TOKENS);
  const budgetMap = {
    low: 4096,
    medium: 10240,
    high: 16384,
    xhigh: 32768,
    max: available,
  };
  const targetBudget = Math.max(
    MIN_THINKING_BUDGET_TOKENS,
    Math.min(budgetMap[normalized] || 10240, available)
  );
  const isClaude = idLower.includes("claude-") || famLower.includes("claude");

  if (protocol === "anthropic" || ((protocol === "bedrock" || protocol === "cline_pass" || protocol === "vertex") && isClaude)) {
    if (available < MIN_THINKING_BUDGET_TOKENS) return { kind: "disabled" };
    return { kind: "anthropic_thinking", budget_tokens: targetBudget };
  }
  if (protocol === "google" || protocol === "vertex") {
    if (available < MIN_THINKING_BUDGET_TOKENS) return { kind: "disabled" };
    return { kind: "google_thinking", thinking_budget: targetBudget };
  }
  return { kind: "open_ai_effort", reasoning_effort: normalized };
}

async function init() {
  try {
    const resp = await fetch("./catalog.json");
    catalog = await resp.json();
  } catch (err) {
    els.meta.innerHTML = `<span class="meta-pill">Failed to load catalog.json: ${err}</span>`;
    return;
  }

  const providers = Object.values(catalog.providers || {});
  allModels = [];
  for (const prov of providers) {
    const opt = document.createElement("option");
    opt.value = prov.id;
    opt.textContent = `${prov.name} (${prov.id}) — ${Object.keys(prov.models || {}).length}`;
    els.providerSelect.appendChild(opt);

    for (const model of Object.values(prov.models || {})) {
      allModels.push({
        ...model,
        _chat: isChatModel(model),
        _protocol: prov.protocol,
        _key: `${prov.id}::${model.id}`,
      });
    }
  }

  els.meta.innerHTML = `
    <span class="meta-pill">${providers.length} Providers</span>
    <span class="meta-pill">${allModels.length} Models</span>
    <span class="meta-pill">Schema v${catalog.schema_version}</span>
  `;

  els.search.addEventListener("input", render);
  els.providerSelect.addEventListener("change", render);
  els.sortSelect.addEventListener("change", render);

  document.querySelectorAll(".filter-chip").forEach((btn) => {
    btn.addEventListener("click", () => {
      const f = btn.dataset.filter;
      if (activeFilters.has(f)) {
        activeFilters.delete(f);
        btn.classList.remove("active");
      } else {
        activeFilters.add(f);
        btn.classList.add("active");
      }
      render();
    });
  });

  render();
  if (allModels.length > 0) {
    selectModel(allModels[0]._key);
  }
}

function render() {
  const q = els.search.value.trim().toLowerCase();
  const provFilter = els.providerSelect.value;
  const sortMode = els.sortSelect.value;

  if (provFilter && catalog.providers[provFilter]) {
    const p = catalog.providers[provFilter];
    els.providerBanner.classList.remove("hidden");
    els.providerBanner.innerHTML = `
      <div>
        <strong>${p.name}</strong> (<code>${p.id}</code>) · Protocol: <code>${p.protocol}</code>
        ${p.requires_streaming ? ' · <span class="badge badge-sub">requires_streaming: true</span>' : ""}
      </div>
      <div>
        Resolved Default Model: <code>${p.default_model_id || "none"}</code>
        ${p.env_vars && p.env_vars.length ? ` · Env: <code>${p.env_vars.join(", ")}</code>` : ""}
      </div>
    `;
  } else {
    els.providerBanner.classList.add("hidden");
  }

  const filtered = allModels.filter((m) => {
    if (provFilter && m.provider_id !== provFilter) return false;
    if (activeFilters.has("recommended") && !m.is_recommended) return false;
    if (activeFilters.has("cline_pass") && m.billing_tier !== "subscription" && !m.id.startsWith("cline-pass/")) return false;
    if (activeFilters.has("free") && m.billing_tier !== "free") return false;
    if (activeFilters.has("chat_only") && !m._chat) return false;
    if (activeFilters.has("reasoning") && !m.supports_reasoning) return false;
    if (activeFilters.has("tool_call") && !m.supports_tool_call) return false;

    if (q) {
      const hay = `${m.provider_id} ${m.id} ${m.name} ${m.family || ""}`.toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });

  filtered.sort((a, b) => {
    if (sortMode === "release_desc") {
      const da = a.release_date || "";
      const db = b.release_date || "";
      if (da !== db) return db.localeCompare(da);
      return a.id.localeCompare(b.id);
    }
    if (sortMode === "context_desc") {
      return (b.context_window || 0) - (a.context_window || 0);
    }
    if (sortMode === "output_desc") {
      return (b.max_tokens || 0) - (a.max_tokens || 0);
    }
    if (sortMode === "cost_asc") {
      const ca = a.cost?.input ?? 999999;
      const cb = b.cost?.input ?? 999999;
      return ca - cb;
    }
    return a.id.localeCompare(b.id);
  });

  els.resultsCount.textContent = `${filtered.length} model${filtered.length === 1 ? "" : "s"}`;

  els.tbody.innerHTML = filtered
    .map((m) => {
      const tierBadge =
        m.billing_tier === "subscription"
          ? `<span class="badge badge-sub">ClinePass</span>`
          : m.billing_tier === "free"
          ? `<span class="badge badge-free">Free</span>`
          : m.billing_tier === "credits"
          ? `<span class="badge badge-credits">Credits</span>`
          : "";
      const recBadge = m.is_recommended ? `<span class="badge badge-rec">★ Recommended</span>` : "";

      const modHtml = m.modalities
        ? `${(m.modalities.input || []).map((x) => `<span class="badge badge-mod">${x}</span>`).join("")} → ${(
            m.modalities.output || []
          )
            .map((x) => `<span class="badge badge-mod">${x}</span>`)
            .join("")}`
        : `<span class="badge badge-mod">default (text → text)</span>`;

      const reasoningHtml = m.supports_reasoning
        ? m.reasoning_options && m.reasoning_options.length
          ? m.reasoning_options.map((r) => `<span class="badge badge-mod">${r}</span>`).join("")
          : `<span class="badge badge-mod">enabled</span>`
        : "—";

      const isSel = m._key === selectedKey ? "selected" : "";

      return `
        <tr data-key="${m._key}" class="${isSel}">
          <td>
            <div class="model-primary">
              <span class="badge badge-provider">${m.provider_id}</span>
              <span>${m.name}</span>
              ${recBadge}
              ${tierBadge}
            </div>
            <div class="model-sub">${m.id}</div>
          </td>
          <td class="mono-num">${fmtNum(m.context_window)} / ${fmtNum(m.max_input_tokens)} / ${fmtNum(m.max_tokens)}</td>
          <td>${modHtml}</td>
          <td>${reasoningHtml}</td>
          <td class="mono-num">${fmtCost(m.cost)}</td>
          <td class="mono-num">${m.release_date || "—"}</td>
        </tr>
      `;
    })
    .join("");

  els.tbody.querySelectorAll("tr").forEach((row) => {
    row.addEventListener("click", () => selectModel(row.dataset.key));
  });
}

function selectModel(key) {
  selectedKey = key;
  els.tbody.querySelectorAll("tr").forEach((r) => {
    r.classList.toggle("selected", r.dataset.key === key);
  });

  const model = allModels.find((m) => m._key === key);
  if (!model) return;

  els.inspectorEmpty.classList.add("hidden");
  els.inspectorContent.classList.remove("hidden");

  els.inspectorContent.innerHTML = `
    <div>
      <div class="model-primary">
        <span class="badge badge-provider">${model.provider_id}</span>
        <h3>${model.name}</h3>
      </div>
      <div class="model-sub" style="margin-top:0.25rem">${model.id}</div>
    </div>

    <div class="inspector-section">
      <h4>Normalized Limits &amp; Classification</h4>
      <div class="kv-grid">
        <div class="kv-box">
          <div class="kv-label">context_window</div>
          <div class="kv-val">${model.context_window?.toLocaleString() ?? "None"}</div>
        </div>
        <div class="kv-box">
          <div class="kv-label">max_input_tokens</div>
          <div class="kv-val">${model.max_input_tokens.toLocaleString()}</div>
        </div>
        <div class="kv-box">
          <div class="kv-label">max_tokens (output)</div>
          <div class="kv-val">${model.max_tokens.toLocaleString()}</div>
        </div>
        <div class="kv-box">
          <div class="kv-label">is_chat_model()</div>
          <div class="kv-val">${model._chat}</div>
        </div>
      </div>
    </div>

    <div class="inspector-section">
      <h4>Gateway Token &amp; Reasoning Simulator</h4>
      <div class="calc-grid">
        <div class="calc-field">
          <label>Estimated Input Tokens</label>
          <input type="number" id="sim-input" value="16000" min="0" step="1000" />
        </div>
        <div class="calc-field">
          <label>Requested Max Output (Optional)</label>
          <input type="number" id="sim-req-out" placeholder="Default (32000)" min="0" step="1024" />
        </div>
        <div class="calc-field">
          <label>Explicit Reasoning Budget</label>
          <input type="number" id="sim-budget" placeholder="Optional floor" min="0" step="1024" />
        </div>
        <div class="calc-field">
          <label>Requested Reasoning Effort</label>
          <select id="sim-effort">
            <option value="none">none</option>
            <option value="low">low</option>
            <option value="medium" selected>medium</option>
            <option value="high">high</option>
            <option value="xhigh">xhigh</option>
            <option value="max">max</option>
          </select>
        </div>
      </div>
      <div id="sim-output"></div>
    </div>

    <div class="inspector-section">
      <h4>Rust Usage Snippet</h4>
      <pre class="code-block">let model = catalog.get_model("${model.provider_id}", "${model.id}").unwrap();
let max_out = model.resolve_gateway_max_output_tokens(16_000, None, None)?;
let wire = model.to_wire_reasoning_params(
    catalog.provider_protocol("${model.provider_id}"),
    ReasoningEffort::High,
    max_out,
);</pre>
    </div>
  `;

  const updateSim = () => {
    const estIn = Number(document.getElementById("sim-input").value || 0);
    const reqOutRaw = document.getElementById("sim-req-out").value;
    const reqOut = reqOutRaw ? Number(reqOutRaw) : null;
    const budgetRaw = document.getElementById("sim-budget").value;
    const budget = budgetRaw ? Number(budgetRaw) : null;
    const effort = document.getElementById("sim-effort").value;

    const gatewayRes = resolveGatewayMaxOutputTokens(model, estIn, reqOut, budget);
    const simOut = document.getElementById("sim-output");

    if (gatewayRes.error) {
      simOut.innerHTML = `<div class="code-block error">${gatewayRes.error}</div>`;
      return;
    }

    const clampedEffort = normalizeEffort(model, effort);
    const wire = toWireReasoningParams(model._protocol, model, effort, gatewayRes.ok);

    simOut.innerHTML = `
      <div class="code-block">resolve_gateway_max_output_tokens => Ok(${gatewayRes.ok.toLocaleString()})
normalize_reasoning_effort("${effort}") => ${clampedEffort ? `"${clampedEffort}"` : "None"}
to_wire_reasoning_params (${model._protocol}) =>
${JSON.stringify(wire, null, 2)}</div>
    `;
  };

  ["sim-input", "sim-req-out", "sim-budget", "sim-effort"].forEach((id) => {
    document.getElementById(id).addEventListener("input", updateSim);
  });
  updateSim();
}

init();
