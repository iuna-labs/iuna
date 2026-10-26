(function () {
  function recommendationFor({ cpu, power, network }) {
    const publicNode = network === "public";
    let mining;
    let burning;

    if (power === "battery") {
      mining = {
        title: "Mining off",
        copy: "Leave mining disabled to avoid continuous proof-of-work use while on battery or saving energy.",
      };
      burning = {
        title: "Burn only, start low",
        copy: "If you already own IUNA, enable Burn with the lowest practical amount. You can still support the burn committee when your mined lineage is eligible.",
      };
    } else if (cpu === "strong") {
      mining = {
        title: "Mining on",
        copy: "Enable Mine and choose a worker count that keeps the computer comfortable for its other tasks.",
      };
      burning = {
        title: "Use the max-EV burn",
        copy: "Use Max-EV in the Burn outlook.",
      };
    } else {
      mining = {
        title: "Mining optional",
        copy: "Mining works on this CPU, but it will be slow. Enable it only if continuous CPU use is acceptable.",
      };
      burning = {
        title: "Keep the burn low",
        copy: "Use the lowest practical amount. Focus on committee participation rather than competing aggressively to finalize blocks.",
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
            copy: "During setup, select Listening node and keep TCP port 9444 reachable through your firewall and router. Expose only P2P; keep the management interface private.",
          }
        : {
            title: "Choose Non-listening node",
            copy: "During setup, select Non-listening node. It connects out to peers and synchronizes normally without an open inbound port.",
          },
      mining,
      burning,
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
    document.querySelector("[data-burn-title]").textContent = result.burning.title;
    document.querySelector("[data-burn-copy]").textContent = result.burning.copy;
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
