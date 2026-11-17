// Behaviour htmx doesn't cover: copying, dragging repositories, local times,
// and holding back live updates while a tag is being typed.
(function () {
  "use strict";

  function board() {
    return document.getElementById("board");
  }

  function editing() {
    const active = document.activeElement;
    return !!(active && active.matches("#board input[type=text]"));
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

  // "Updated 5 minutes ago", with the exact time on hover.
  function ago(seconds) {
    const plural = (n, unit) => `${n} ${unit}${n === 1 ? "" : "s"} ago`;
    if (seconds < 60) return "just now";
    const minutes = Math.floor(seconds / 60);
    if (minutes < 60) return plural(minutes, "minute");
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return plural(hours, "hour");
    return plural(Math.floor(hours / 24), "day");
  }

  function localTimes() {
    document.querySelectorAll("time[data-epoch]").forEach((el) => {
      const date = new Date(Number(el.dataset.epoch) * 1000);
      el.textContent = ago(Math.max(0, (Date.now() - date) / 1000));
      el.title = date.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" });
    });
  }
  localTimes();
  // The header's time arrives out of band, which htmx.onLoad doesn't report.
  document.addEventListener("htmx:oobAfterSwap", localTimes);
  document.addEventListener("htmx:afterSettle", localTimes);
  setInterval(localTimes, 15000);

  // Copying. Each section carries its text in both formats; a button copies
  // its own section plus any checked ones, in page order. When several are
  // copied the untagged section is headed "Other", so its pull requests don't
  // read as part of the group above them. Both clipboard flavours go on, so
  // Slack pastes real links and a plain text editor still gets something
  // readable.
  const selected = new Set();

  function sections() {
    return Array.from(document.querySelectorAll("section.tag-section"));
  }

  // The sections `button` would copy.
  function targets(button) {
    const own = button.closest("section.tag-section");
    return sections().filter((section) => section === own || selected.has(section.dataset.tag));
  }

  function label(button) {
    const n = selected.size ? targets(button).length : 0;
    button.textContent = n ? button.dataset.labelSelected.replace("{n}", n) : button.dataset.label;
  }

  function syncSelection() {
    const present = new Set(sections().map((section) => section.dataset.tag));
    for (const tag of selected) if (!present.has(tag)) selected.delete(tag);
    sections().forEach((section) => {
      const box = section.querySelector("input.select-section");
      if (box) box.checked = selected.has(section.dataset.tag);
    });
    document.querySelectorAll("button.copy").forEach(label);
  }
  // A live update replaces the board; keep what was checked.
  htmx.onLoad(syncSelection);

  document.addEventListener("change", (event) => {
    const box = event.target;
    if (!box.matches || !box.matches("input.select-section")) return;
    const tag = box.closest("section.tag-section").dataset.tag;
    if (box.checked) selected.add(tag);
    else selected.delete(tag);
    syncSelection();
  });

  document.addEventListener("click", async (event) => {
    const button = event.target.closest("button.copy");
    if (!button) return;
    const chosen = targets(button);
    const [textKey, htmlKey] = button.dataset.mode === "repos" ? ["textRepos", "htmlRepos"] : ["text", "html"];
    const headed = (section, heading, body) =>
      chosen.length > 1 && section.dataset.tag === "" ? heading + body : body;
    const text = chosen.map((section) => headed(section, "*Other*\n", section.dataset[textKey])).join("\n\n");
    const html = chosen.map((section) => headed(section, "<p><b>Other</b></p>", section.dataset[htmlKey])).join("");
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
      selected.clear();
      syncSelection();
      flash(button, chosen.length > 1 ? `Copied ${chosen.length} groups ✓` : "Copied ✓");
    } catch (error) {
      console.error(error);
      flash(button, "Copy failed");
    }
  });

  function flash(button, text) {
    button.textContent = text;
    setTimeout(() => label(button), 1500);
  }

  // The tag field's suggestions. Focusing the field lists the tags with open
  // work on the page; typing filters them. Clicking a suggestion, or Enter on
  // a highlighted one, applies it at once. Tab copies the highlighted one into
  // the field instead, to be edited into a new tag.
  const picker = { input: null, menu: null, items: [], index: -1, typed: false };

  function liveTags() {
    try {
      return JSON.parse(board().dataset.tags || "[]");
    } catch (error) {
      return [];
    }
  }

  function openPicker(input) {
    if (picker.input !== input) {
      closePicker();
      picker.input = input;
      picker.typed = false;
    }
    picker.index = -1;
    renderPicker();
  }

  function closePicker() {
    if (picker.menu) picker.menu.remove();
    if (picker.input) {
      picker.input.setAttribute("aria-expanded", "false");
      picker.input.removeAttribute("aria-activedescendant");
    }
    picker.input = null;
    picker.menu = null;
    picker.items = [];
    picker.index = -1;
  }

  function renderPicker() {
    const input = picker.input;
    const needle = picker.typed ? input.value.trim().toLowerCase() : "";
    picker.items = liveTags().filter((tag) => tag.toLowerCase().includes(needle));
    if (picker.index >= picker.items.length) picker.index = picker.items.length - 1;

    if (picker.items.length === 0) {
      if (picker.menu) picker.menu.remove();
      picker.menu = null;
      input.setAttribute("aria-expanded", "false");
      input.removeAttribute("aria-activedescendant");
      return;
    }
    if (!picker.menu) {
      picker.menu = document.createElement("ul");
      picker.menu.className = "tag-menu";
      picker.menu.id = "tag-menu";
      picker.menu.setAttribute("role", "listbox");
      input.closest("form").appendChild(picker.menu);
      input.setAttribute("aria-controls", "tag-menu");
    }
    picker.menu.replaceChildren(
      ...picker.items.map((tag, i) => {
        const li = document.createElement("li");
        li.id = `tag-option-${i}`;
        li.setAttribute("role", "option");
        li.dataset.tag = tag;
        li.textContent = tag;
        if (i === picker.index) {
          li.classList.add("active");
          li.setAttribute("aria-selected", "true");
        }
        return li;
      }),
    );
    input.setAttribute("aria-expanded", "true");
    if (picker.index >= 0) {
      input.setAttribute("aria-activedescendant", `tag-option-${picker.index}`);
      picker.menu.children[picker.index].scrollIntoView({ block: "nearest" });
    } else {
      input.removeAttribute("aria-activedescendant");
    }
  }

  function applyTag(input, tag) {
    input.value = tag;
    const form = input.closest("form");
    closePicker();
    form.requestSubmit();
  }

  function isTagInput(el) {
    return !!(el && el.matches && el.matches("form.tag input[name=tag]"));
  }

  document.addEventListener("focusin", (event) => {
    if (isTagInput(event.target)) openPicker(event.target);
  });
  // A click on a field that already has focus, after Escape closed its list.
  document.addEventListener("click", (event) => {
    if (isTagInput(event.target) && !picker.menu) openPicker(event.target);
  });
  document.addEventListener("focusout", (event) => {
    if (event.target === picker.input) closePicker();
  });
  document.addEventListener("input", (event) => {
    if (event.target !== picker.input) return;
    picker.typed = true;
    picker.index = -1;
    renderPicker();
  });
  // Keep focus in the field while a suggestion is clicked.
  document.addEventListener("mousedown", (event) => {
    if (event.target.closest && event.target.closest(".tag-menu")) event.preventDefault();
  });
  document.addEventListener("click", (event) => {
    const option = event.target.closest && event.target.closest(".tag-menu li");
    if (option && picker.input) applyTag(picker.input, option.dataset.tag);
  });
  document.addEventListener("keydown", (event) => {
    const input = event.target;
    if (!isTagInput(input)) return;
    if (picker.input !== input) openPicker(input);
    const highlighted = picker.menu && picker.index >= 0 ? picker.items[picker.index] : null;
    const step = (move) => {
      event.preventDefault();
      if (!picker.menu) {
        picker.index = -1;
        renderPicker();
      }
      const count = picker.items.length;
      if (count) picker.index = move(picker.index, count);
      renderPicker();
    };
    switch (event.key) {
      case "ArrowDown":
        step((i, count) => (i + 1) % count);
        break;
      case "ArrowUp":
        step((i, count) => (i <= 0 ? count - 1 : i - 1));
        break;
      case "Tab":
        if (highlighted !== null && !event.shiftKey) {
          event.preventDefault();
          input.value = highlighted;
          picker.typed = true;
          picker.index = -1;
          if (picker.menu) picker.menu.remove();
          picker.menu = null;
          input.setAttribute("aria-expanded", "false");
          input.removeAttribute("aria-activedescendant");
        }
        break;
      case "Enter":
        if (highlighted !== null) {
          event.preventDefault();
          applyTag(input, highlighted);
        }
        break;
      case "Escape":
        if (picker.menu) {
          event.preventDefault();
          picker.menu.remove();
          picker.menu = null;
          input.setAttribute("aria-expanded", "false");
        }
        break;
    }
  });

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
