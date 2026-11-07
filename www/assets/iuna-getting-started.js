(function () {
  function recommendationFor({ cpu, power, network }) {
    const publicNode = network === "public";
    let mining;
    let ticketMining;

    if (power === "battery") {
      mining = {
        title: "Hash mining off",
        copy: "Run only the validating node while on battery or saving energy. It does not perform continuous Proof-of-Work by itself.",
      };
      ticketMining = {
        title: "Ticket mining optional",
        copy: "A ticket does not require continuous hashing, but its expenditure is irreversible. Start with a short duration and the quoted minimum budget.",
      };
    } else if (cpu === "strong") {
      mining = {
        title: "Hash miner not packaged yet",
        copy: "The source preview validates Heartbeats, but does not yet expose an automatic Hash-mining worker. Use this device as a validating node for now.",
      };
      ticketMining = {
        title: "Quote a mining ticket",
        copy: "Choose an active duration in the Mining screen and use its current minimum expenditure quote as a conservative starting point.",
      };
    } else {
      mining = {
        title: "Run a validating node",
        copy: "The source preview does not yet expose an automatic Hash-mining worker. Validation and P2P relay still help test this device class.",
      };
      ticketMining = {
        title: "Ticket mining works",
        copy: "Ledger ticket selection does not depend on a fast CPU. Start with a short duration and the quoted minimum expenditure budget.",
      };
    }

    return {
      summary: [
        cpu === "strong" ? "Modern CPU" : "Lightweight CPU",
        power === "battery" ? "Energy-saving" : "Plugged in",
        publicNode ? "Public P2P" : "Outbound only",
      ],
      network: publicNode
        ? {
            title: "Choose Listening node",
            copy: "Keep TCP port 19733 reachable through your firewall and router. Expose only P2P; keep the operator interface on loopback.",
          }
        : {
            title: "Choose Non-listening node",
            copy: "Bind P2P locally or behind your firewall and configure outbound peers. The operator interface must remain private.",
          },
      mining,
      ticketMining,
    };
  }

  window.iunaGettingStartedRecommendation = recommendationFor;
  if (typeof document === "undefined") return;

  const groups = Array.from(document.querySelectorAll("[data-choice-group]"));
  const configurator = document.querySelector(".configurator");
  const choicesView = document.querySelector("[data-choices-view]");
  const recommendation = document.querySelector("[data-recommendation]");
  const next = document.querySelector("[data-next]");
  if (!groups.length || !configurator || !choicesView || !recommendation || !next) return;

  const state = { cpu: null, power: null, network: null };
  const progress = document.querySelector("[data-progress]");

  function render() {
    const completed = Object.values(state).filter(Boolean).length;
    progress.textContent = `${completed} of 3`;
    next.hidden = completed < 3;
  }

  function showRecommendation() {
    const result = recommendationFor(state);
    document.querySelector("[data-summary]").innerHTML = result.summary
      .map((item) => `<span>${item}</span>`)
      .join("");
    document.querySelector("[data-network-title]").textContent = result.network.title;
    document.querySelector("[data-network-copy]").textContent = result.network.copy;
    document.querySelector("[data-mine-title]").textContent = result.mining.title;
    document.querySelector("[data-mine-copy]").textContent = result.mining.copy;
    document.querySelector("[data-ticket-title]").textContent = result.ticketMining.title;
    document.querySelector("[data-ticket-copy]").textContent = result.ticketMining.copy;
    choicesView.hidden = true;
    recommendation.hidden = false;
    configurator.classList.add("is-plan");
    configurator.scrollIntoView({ behavior: "smooth", block: "start" });
    document.querySelector("#recommendation-title").focus({ preventScroll: true });
  }

  groups.forEach((group) => {
    const key = group.dataset.choiceGroup;
    const buttons = Array.from(group.querySelectorAll("[data-value]"));
    buttons.forEach((button) => {
      button.addEventListener("click", () => {
        state[key] = button.dataset.value;
        buttons.forEach((candidate) => {
          candidate.setAttribute("aria-pressed", String(candidate === button));
        });
        render();
      });
    });
  });

  next.addEventListener("click", showRecommendation);

  document.querySelector("[data-reset]").addEventListener("click", () => {
    recommendation.hidden = true;
    choicesView.hidden = false;
    configurator.classList.remove("is-plan");
    configurator.scrollIntoView({ behavior: "smooth", block: "start" });
    document.querySelector("#configurator-title").focus({ preventScroll: true });
  });
})();
