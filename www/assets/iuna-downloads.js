(function () {
  var rawVersion = window.IUNA_DOWNLOADS_VERSION || "";
  var version = rawVersion.replace(/^v/, "");
  var tag = "v" + version;
  var base = "/downloads/";

  var artifacts = [
    {
      label: "Linux CLI",
      title: "Linux x86_64",
      description: "Command-line node and wallet UI server.",
      file: "iuna-" + tag + "-linux-x86_64.tar.gz",
      button: "Download tar.gz"
    },
    {
      label: "Linux CLI",
      title: "Linux aarch64",
      description: "Command-line node and wallet UI server for ARM64 Linux.",
      file: "iuna-" + tag + "-linux-aarch64.tar.gz",
      button: "Download tar.gz"
    },
    {
      label: "macOS desktop",
      title: "Apple silicon",
      description: "Desktop app bundle for macOS.",
      file: "iuna-" + tag + "-macos-aarch64-desktop.app.zip",
      button: "Download app.zip"
    },
    {
      label: "Windows desktop",
      title: "Windows x86_64",
      description: "Desktop installer for Windows.",
      file: "iuna-" + tag + "-windows-x86_64-desktop-setup.exe",
      button: "Download setup.exe"
    },
    {
      label: "Checksums",
      title: "SHA256SUMS",
      description: "SHA-256 checksums for available files.",
      file: "SHA256SUMS",
      button: "Download checksums",
      hideCard: true
    }
  ];

  function escapeHtml(value) {
    return String(value).replace(/[&<>"']/g, function (character) {
      return {
        "&": "&amp;",
        "<": "&lt;",
        ">": "&gt;",
        '"': "&quot;",
        "'": "&#39;"
      }[character];
    });
  }

  function formatBytes(value) {
    if (!Number.isFinite(value) || value <= 0) {
      return "unknown";
    }

    var units = ["B", "KB", "MB", "GB"];
    var size = value;
    var unit = 0;
    while (size >= 1024 && unit < units.length - 1) {
      size /= 1024;
      unit += 1;
    }

    return new Intl.NumberFormat(undefined, {
      maximumFractionDigits: size >= 10 || unit === 0 ? 0 : 1
    }).format(size) + " " + units[unit];
  }

  function checkArtifact(artifact) {
    return fetch(base + artifact.file, { method: "HEAD", cache: "no-store" })
      .then(function (response) {
        if (!response.ok) {
          return null;
        }

        return Object.assign({}, artifact, {
          size: Number(response.headers.get("content-length"))
        });
      })
      .catch(function () {
        return null;
      });
  }

  function renderCards(files) {
    var container = document.querySelector("[data-download-cards]");
    if (!container) {
      return;
    }

    var cards = files.filter(function (file) {
      return !file.hideCard;
    });

    if (cards.length === 0) {
      container.innerHTML = '<article class="card"><span class="tag pending">Pending</span><h2>No artifacts yet</h2><p class="muted">Release files have not been uploaded for this version.</p></article>';
      return;
    }

    container.innerHTML = cards.map(function (file) {
      return [
        '<article class="card">',
        '<span class="tag">' + escapeHtml(file.label) + '</span>',
        '<h2>' + escapeHtml(file.title) + '</h2>',
        '<p>' + escapeHtml(file.description) + '</p>',
        '<p><a class="button" href="' + escapeHtml(base + file.file) + '">' + escapeHtml(file.button) + '</a></p>',
        '</article>'
      ].join("");
    }).join("");
  }

  function renderFiles(files) {
    var body = document.querySelector("[data-download-files]");
    if (!body) {
      return;
    }

    if (files.length === 0) {
      body.innerHTML = '<tr><td colspan="2" class="muted">No files found.</td></tr>';
      return;
    }

    body.innerHTML = files.map(function (file) {
      return '<tr><td><a href="' + escapeHtml(base + file.file) + '">' + escapeHtml(file.file) + '</a></td><td>' + escapeHtml(formatBytes(file.size)) + '</td></tr>';
    }).join("");
  }

  function hydrateStaticText() {
    var versionNodes = document.querySelectorAll("[data-download-version]");
    var linuxFile = "iuna-" + tag + "-linux-x86_64.tar.gz";
    var linuxNodes = document.querySelectorAll("[data-linux-x86-file]");

    versionNodes.forEach(function (node) {
      node.textContent = tag;
    });
    linuxNodes.forEach(function (node) {
      node.textContent = linuxFile;
    });
  }

  hydrateStaticText();
  Promise.all(artifacts.map(checkArtifact)).then(function (results) {
    var files = results.filter(Boolean);
    renderCards(files);
    renderFiles(files);
  });
}());
