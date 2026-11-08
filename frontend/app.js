let activeChallengeId = "";
let currentSelectedAccountId = null;
let countdownInterval = null;
let currentFilter = "all";
let cachedStatuses = [];

document.addEventListener("DOMContentLoaded", () => {
    loadAccounts();
    loadAllVoteStatus();

    document.getElementById("btnSendOtp").addEventListener("click", handleSendOtp);
    document.getElementById("btnVerifyOtp").addEventListener("click", handleVerifyOtp);
    document.getElementById("selectAccount").addEventListener("change", handleAccountSelected);
    document.getElementById("btnStartScrape").addEventListener("click", handleStartScrape);
    document.getElementById("btnResetCooldown").addEventListener("click", handleResetCooldown);

    // Filters
    document.getElementById("filterAll").addEventListener("click", () => setFilter("all"));
    document.getElementById("filterEligible").addEventListener("click", () => setFilter("eligible"));
    document.getElementById("filterVoted").addEventListener("click", () => setFilter("voted"));
    document.getElementById("btnSyncAll").addEventListener("click", handleSyncAll);

    // Auto-sync dashboard every 15 seconds to reflect background auto-claims
    setInterval(async () => {
        await loadAllVoteStatus();
        if (currentSelectedAccountId) {
            await refreshCooldownStatus(currentSelectedAccountId);
            await loadHistory(currentSelectedAccountId);
        }
    }, 15000);
});

function setFilter(filter) {
    currentFilter = filter;
    ["filterAll", "filterEligible", "filterVoted"].forEach(id => {
        document.getElementById(id).classList.remove("btn-gold");
    });

    if (filter === "all") document.getElementById("filterAll").classList.add("btn-gold");
    if (filter === "eligible") document.getElementById("filterEligible").classList.add("btn-gold");
    if (filter === "voted") document.getElementById("filterVoted").classList.add("btn-gold");

    renderEligibilityTable(cachedStatuses);
}

async function handleSyncAll() {
    const btn = document.getElementById("btnSyncAll");
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> Syncing...`;

    try {
        const res = await fetch("/api/accounts/sync-all", { method: "POST" });
        const data = await res.json();
        if (data.success) {
            await loadAllVoteStatus();
            if (currentSelectedAccountId) {
                await refreshCooldownStatus(currentSelectedAccountId);
                await loadHistory(currentSelectedAccountId);
            }
        }
    } catch (err) {
        alert("Failed to sync live status.");
    } finally {
        btn.disabled = false;
        btn.textContent = "🔄 Sync Live Status";
    }
}

async function loadAccounts() {
    try {
        const res = await fetch("/api/accounts");
        const data = await res.json();
        if (data.success) {
            renderAccountsTable(data.accounts);
            populateAccountDropdown(data.accounts);
        }
    } catch (err) {
        console.error("Failed to load accounts:", err);
    }
}

function renderAccountsTable(accounts) {
    const tbody = document.querySelector("#accountsTable tbody");
    if (!accounts || accounts.length === 0) {
        tbody.innerHTML = `<tr><td colspan="5" class="text-center">No accounts registered yet.</td></tr>`;
        return;
    }

    tbody.innerHTML = accounts.map(acc => {
        const labelText = acc.label || "";
        return `
        <tr>
            <td><strong>#${acc.id}</strong></td>
            <td>
                <input type="text" class="label-input" id="label_${acc.id}" value="${labelText}" placeholder="Mom, Dad, Self..." style="padding: 4px 8px; font-size: 0.85rem; width: 110px;">
            </td>
            <td><code>${acc.phone_number}</code></td>
            <td><span class="status-badge ${acc.registration_status === 'Verified' ? 'badge-green' : 'badge-orange'}">${acc.registration_status}</span></td>
            <td>
                <button class="btn btn-primary btn-sm" onclick="saveLabel(${acc.id})" style="padding: 4px 8px; font-size: 0.8rem;">Save</button>
                <button class="btn btn-danger btn-sm" onclick="deleteAccount(${acc.id}, '${acc.phone_number}')" style="padding: 4px 8px; font-size: 0.8rem; margin-left: 4px;">Delete</button>
            </td>
        </tr>
    `}).join("");
}

async function deleteAccount(accountId, phoneNumber) {
    if (!confirm(`Are you sure you want to delete account #${accountId} (${phoneNumber})?`)) {
        return;
    }

    try {
        const res = await fetch(`/api/accounts/${accountId}`, {
            method: "DELETE"
        });
        const data = await res.json();
        if (data.success) {
            await loadAccounts();
            await loadAllVoteStatus();
        } else {
            alert(data.message || "Failed to delete account.");
        }
    } catch (err) {
        alert("Failed to delete account due to a network error.");
    }
}

async function saveLabel(accountId) {
    const input = document.getElementById(`label_${accountId}`);
    const label = input.value.trim();

    try {
        const res = await fetch(`/api/accounts/${accountId}/label`, {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ label })
        });
        const data = await res.json();
        if (data.success) {
            alert(`Label saved: "${label || "None"}"`);
            await loadAccounts();
            await loadAllVoteStatus();
        }
    } catch (err) {
        alert("Failed to update label.");
    }
}

async function loadAllVoteStatus() {
    try {
        const res = await fetch("/api/accounts/vote-status");
        const data = await res.json();
        if (data.success) {
            cachedStatuses = data.accounts_status;
            renderEligibilityTable(cachedStatuses);
        }
    } catch (err) {
        console.error("Failed to load vote status:", err);
    }
}

function renderEligibilityTable(statuses) {
    const tbody = document.querySelector("#eligibilityTable tbody");
    if (!statuses || statuses.length === 0) {
        tbody.innerHTML = `<tr><td colspan="4" class="text-center">No accounts available.</td></tr>`;
        return;
    }

    let filtered = statuses;
    if (currentFilter === "eligible") {
        filtered = statuses.filter(s => s.is_eligible);
    } else if (currentFilter === "voted") {
        filtered = statuses.filter(s => !s.is_eligible);
    }

    if (filtered.length === 0) {
        tbody.innerHTML = `<tr><td colspan="4" class="text-center">No accounts match this filter.</td></tr>`;
        return;
    }

    tbody.innerHTML = filtered.map(s => {
        const displayName = s.label ? `<strong>${s.label}</strong> <small style="color:#8b949e">(#${s.account_id})</small>` : `Account #${s.account_id}`;
        const statusBadge = s.is_eligible
            ? `<span class="status-badge badge-green">🟢 Eligible (Ready to Vote)</span>`
            : `<span class="status-badge badge-orange">🟡 Already Voted / Cooldown</span>`;

        let nextTimeStr = "Ready Now";
        if (!s.is_eligible) {
            const dateStr = s.next_eligible_at ? new Date(s.next_eligible_at).toLocaleTimeString() : "In 12 Hours";
            const hours = Math.floor(s.remaining_seconds / 3600);
            const mins = Math.floor((s.remaining_seconds % 3600) / 60);
            nextTimeStr = `${dateStr} (${hours}h ${mins}m left)`;
        }

        return `
            <tr style="cursor: pointer;" onclick="quickSelectAccount(${s.account_id})">
                <td>${displayName}</td>
                <td><code>${s.phone_number}</code></td>
                <td>${statusBadge}</td>
                <td>${nextTimeStr}</td>
            </tr>
        `;
    }).join("");
}

function quickSelectAccount(accountId) {
    const select = document.getElementById("selectAccount");
    select.value = accountId;
    handleAccountSelected();
}

function populateAccountDropdown(accounts) {
    const select = document.getElementById("selectAccount");
    const prevVal = select.value;
    select.innerHTML = `<option value="">-- Choose a registered phone number --</option>`;

    accounts.forEach(acc => {
        const opt = document.createElement("option");
        opt.value = acc.id;
        const namePrefix = acc.label ? `[${acc.label}] ` : "";
        opt.textContent = `${namePrefix}${acc.phone_number} (#${acc.id}) [${acc.registration_status}]`;
        select.appendChild(opt);
    });

    if (prevVal) {
        select.value = prevVal;
    }
}

async function handleSendOtp() {
    const phoneInput = document.getElementById("phoneNumber");
    const phone = phoneInput.value.trim();
    if (!phone) {
        alert("Please enter a valid phone number (e.g., 0946692896 or +2519...)");
        return;
    }

    const btn = document.getElementById("btnSendOtp");
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> Sending...`;

    try {
        const res = await fetch("/api/accounts/request-otp", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ phone_number: phone })
        });
        const data = await res.json();

        if (res.ok && data.success) {
            activeChallengeId = data.challenge_id;
            document.getElementById("otpSection").classList.remove("hidden");
            document.getElementById("otpStatusMsg").textContent = `Code sent to ${phone}. Enter the 6 digits:`;
        } else if (res.status === 409 || data.error?.code === "ACCOUNT_ALREADY_EXISTS") {
            alert(`ℹ️ Phone number is already registered and active!\n\nSwitching to this account in your dashboard.`);
            document.getElementById("otpSection").classList.add("hidden");
            phoneInput.value = "";
            await loadAccounts();
            await loadAllVoteStatus();
            
            // Auto-select this account
            const select = document.getElementById("selectAccount");
            for (let i = 0; i < select.options.length; i++) {
                if (select.options[i].text.includes(phone) || select.options[i].text.includes(phone.replace("+251", "0"))) {
                    select.selectedIndex = i;
                    handleAccountSelected();
                    break;
                }
            }
        } else {
            alert(`Error: ${data.error?.message || "Failed to send OTP"}`);
        }
    } catch (err) {
        alert("Network error communicating with backend server.");
    } finally {
        btn.disabled = false;
        btn.textContent = "Send OTP";
    }
}

async function handleVerifyOtp() {
    const phone = document.getElementById("phoneNumber").value.trim();
    const code = document.getElementById("otpCode").value.trim();

    if (!code || code.length !== 6) {
        alert("Please enter the 6-digit verification code.");
        return;
    }

    const btn = document.getElementById("btnVerifyOtp");
    btn.disabled = true;
    btn.innerHTML = `<span class="spinner"></span> Verifying...`;

    try {
        const res = await fetch("/api/accounts/verify-otp", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({
                phone_number: phone,
                otp_code: code,
                challenge_id: activeChallengeId
            })
        });
        const data = await res.json();

        if (res.ok && data.success) {
            alert("Verification successful! Session encrypted and saved at rest.");
            document.getElementById("otpSection").classList.add("hidden");
            document.getElementById("otpCode").value = "";
            document.getElementById("phoneNumber").value = "";
            await loadAccounts();
            await loadAllVoteStatus();

            document.getElementById("selectAccount").value = data.account.id;
            handleAccountSelected();
        } else {
            alert(`Verification failed: ${data.error?.message || "Invalid OTP code"}`);
        }
    } catch (err) {
        alert("Network error verifying OTP.");
    } finally {
        btn.disabled = false;
        btn.textContent = "Verify & Save Session";
    }
}

async function handleAccountSelected() {
    const select = document.getElementById("selectAccount");
    const accountId = select.value;
    currentSelectedAccountId = accountId;

    if (!accountId) {
        document.getElementById("cooldownCard").classList.add("hidden");
        document.getElementById("btnStartScrape").disabled = true;
        if (countdownInterval) clearInterval(countdownInterval);
        return;
    }

    await refreshCooldownStatus(accountId);
    await loadHistory(accountId);
}

async function refreshCooldownStatus(accountId) {
    if (countdownInterval) clearInterval(countdownInterval);

    try {
        const res = await fetch(`/api/scrape/status/${accountId}`);
        const data = await res.json();

        if (data.success) {
            updateCooldownUi(data.cooldown);
        }
    } catch (err) {
        console.error("Failed to check cooldown status:", err);
    }
}

function updateCooldownUi(cooldown) {
    const card = document.getElementById("cooldownCard");
    const badge = document.getElementById("cooldownBadge");
    const timerText = document.getElementById("cooldownTimerText");
    const explanation = document.getElementById("cooldownExplanation");
    const startBtn = document.getElementById("btnStartScrape");

    card.classList.remove("hidden");

    if (cooldown.is_eligible) {
        badge.className = "status-badge badge-green";
        badge.textContent = "Eligible";
        timerText.textContent = "00:00:00";
        explanation.textContent = "12-hour interval satisfied. Ready to vote/claim!";
        startBtn.disabled = false;
    } else {
        badge.className = "status-badge badge-orange";
        badge.textContent = "Cooldown Active";
        startBtn.disabled = true;

        let remaining = cooldown.remaining_seconds;
        renderTimer(remaining, timerText);

        countdownInterval = setInterval(() => {
            remaining -= 1;
            if (remaining <= 0) {
                clearInterval(countdownInterval);
                refreshCooldownStatus(currentSelectedAccountId);
                loadAllVoteStatus();
            } else {
                renderTimer(remaining, timerText);
            }
        }, 1000);

        const nextDate = new Date(cooldown.next_eligible_at).toLocaleTimeString();
        explanation.textContent = `12-Hour cooldown active. Next eligible scrape at ${nextDate}.`;
    }
}

function renderTimer(totalSeconds, displayElem) {
    const hours = Math.floor(totalSeconds / 3600);
    const minutes = Math.floor((totalSeconds % 3600) / 60);
    const seconds = totalSeconds % 60;

    const pad = n => String(n).padStart(2, "0");
    displayElem.textContent = `${pad(hours)}:${pad(minutes)}:${pad(seconds)}`;
}

async function handleStartScrape() {
    if (!currentSelectedAccountId) return;

    const btn = document.getElementById("btnStartScrape");
    const progress = document.getElementById("scrapeProgress");

    btn.disabled = true;
    progress.classList.remove("hidden");

    try {
        const res = await fetch(`/api/scrape/start/${currentSelectedAccountId}`, {
            method: "POST"
        });
        const data = await res.json();

        if (res.ok && data.success) {
            alert(`Claim operation finished! Records/Points: ${data.record.records_scraped}`);
        } else {
            alert(`Blocked or failed: ${data.error?.message || "Operation failed"}`);
        }
    } catch (err) {
        alert("Network error triggering scrape.");
    } finally {
        progress.classList.add("hidden");
        await refreshCooldownStatus(currentSelectedAccountId);
        await loadHistory(currentSelectedAccountId);
        await loadAllVoteStatus();
    }
}

async function handleResetCooldown() {
    if (!currentSelectedAccountId) {
        alert("Please select an account first.");
        return;
    }
    try {
        const res = await fetch(`/api/scrape/reset/${currentSelectedAccountId}`, { method: "POST" });
        const data = await res.json();
        if (data.success) {
            alert("Cooldown reset! You can now test claiming votes immediately.");
            await refreshCooldownStatus(currentSelectedAccountId);
            await loadHistory(currentSelectedAccountId);
            await loadAllVoteStatus();
        }
    } catch (err) {
        alert("Failed to reset cooldown.");
    }
}

async function loadHistory(accountId) {
    try {
        const res = await fetch(`/api/scrape/history/${accountId}`);
        const data = await res.json();
        if (data.success) {
            renderHistoryTable(data.history);
        }
    } catch (err) {
        console.error("Failed to load history:", err);
    }
}

function renderHistoryTable(history) {
    const tbody = document.querySelector("#historyTable tbody");
    if (!history || history.length === 0) {
        tbody.innerHTML = `<tr><td colspan="5" class="text-center">No scraping history for this account yet.</td></tr>`;
        return;
    }

    tbody.innerHTML = history.map(rec => `
        <tr>
            <td><strong>#${rec.id}</strong></td>
            <td>${new Date(rec.scraping_started_at).toLocaleString()}</td>
            <td><span class="status-badge ${rec.scraping_status === 'Success' ? 'badge-green' : (rec.scraping_status === 'Running' ? 'badge-blue' : 'badge-orange')}">${rec.scraping_status}</span></td>
            <td><strong>${rec.records_scraped}</strong></td>
            <td>${rec.error_message ? `<span style="color: #da3633">${rec.error_message}</span>` : 'Completed'}</td>
        </tr>
    `).join("");
}
