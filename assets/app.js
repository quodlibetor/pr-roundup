// Behaviour htmx doesn't cover: copying, dragging repositories, local times,
// and holding back live updates while a tag is being typed.
(function () {
  "use strict";

  function board() {
    return document.getElementById("board");
  }

  function editing() {
    const active = document.activeElement;
    return !!(active && active.matches("#board input"));
  }

  // A live update would replace the input being typed into, so it waits
  // until focus leaves the board's inputs.
  let deferred = false;
  document.addEventListener("htmx:beforeRequest", (event) => {
    const elt = event.detail.elt;
    if (elt.id === "board" && event.detail.requestConfig.verb === "get" && editing()) {
      event.preventDefault();
      deferred = true;
    }
  });
  document.addEventListener("focusout", () => {
    setTimeout(() => {
      if (deferred && !editing()) {
        deferred = false;
        htmx.trigger(board(), "reload");
      }
    }, 0);
  });

  // After the server restarts the event stream reconnects; catch up on what
  // happened while it was gone.
  document.addEventListener("htmx:sseOpen", () => {
    if (board()) htmx.trigger(board(), "reload");
  });

  function localTimes(root) {
    root.querySelectorAll("time[data-epoch]").forEach((el) => {
      const date = new Date(Number(el.dataset.epoch) * 1000);
      el.textContent = date.toLocaleTimeString();
      el.title = date.toLocaleString();
    });
  }
  htmx.onLoad(localTimes);

  // Copy a section with both flavours, so Slack pastes real links and a plain
  // text editor still gets something readable.
  document.addEventListener("click", async (event) => {
    const button = event.target.closest("button.copy");
    if (!button) return;
    const { text, html } = button.dataset;
    try {
      if (window.ClipboardItem) {
        await navigator.clipboard.write([
          new ClipboardItem({
            "text/html": new Blob([html], { type: "text/html" }),
            "text/plain": new Blob([text], { type: "text/plain" }),
          }),
        ]);
      } else {
        await navigator.clipboard.writeText(text);
      }
      flash(button, "Copied ✓");
    } catch (error) {
      console.error(error);
      flash(button, "Copy failed");
    }
  });

  function flash(button, label) {
    const original = button.dataset.label || button.textContent;
    button.dataset.label = original;
    button.textContent = label;
    setTimeout(() => {
      button.textContent = original;
    }, 1500);
  }

  // Drag a repository's heading to move it within its tag section.
  let dragged = null;
  document.addEventListener("dragstart", (event) => {
    const head = event.target.closest && event.target.closest(".repo-head");
    if (!head) return;
    dragged = head.closest(".repo");
    dragged.classList.add("dragging");
    event.dataTransfer.effectAllowed = "move";
    event.dataTransfer.setData("text/plain", dragged.dataset.repo);
  });
  document.addEventListener("dragover", (event) => {
    if (!dragged) return;
    const over = event.target.closest && event.target.closest(".repo");
    if (!over || over === dragged || over.parentElement !== dragged.parentElement) return;
    event.preventDefault();
    const rect = over.getBoundingClientRect();
    const after = event.clientY > rect.top + rect.height / 2;
    over.parentElement.insertBefore(dragged, after ? over.nextSibling : over);
  });
  document.addEventListener("drop", (event) => {
    if (dragged) event.preventDefault();
  });
  document.addEventListener("dragend", async () => {
    if (!dragged) return;
    const container = dragged.parentElement;
    dragged.classList.remove("dragging");
    dragged = null;
    const tag = container.closest(".tag-section").dataset.tag;
    const repos = Array.from(container.querySelectorAll(":scope > .repo")).map((el) => el.dataset.repo);
    await fetch("/order", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ tag, repos }),
    });
  });
})();
