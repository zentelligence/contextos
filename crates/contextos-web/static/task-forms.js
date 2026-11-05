// Shared behaviour between the Tasks screen's detail/edit view and its
// creation view: both use the same four-field task-to-task link-chip
// picker (`templates/task_link_fields.html`), so both wire the identical
// chip add/remove behaviour rather than each carrying its own copy.
//
// This is the one genuinely irreducible piece of client-side behaviour on
// either screen: every other field is a plain named input the server reads
// straight off the submitted form (`hx-ext="json-enc"`, no `hx-vals`
// anywhere), but a live search-and-pick chip list has no plain-HTML
// equivalent. Even here, this script only ever adds or removes a chip's own
// `<span>` (and the hidden `name="{{ field.key }}"` input nested inside
// it, submitted verbatim, in whatever raw form the search result carries)
// - it never builds JSON, never decides what a value means, and never
// duplicates a rule the server already owns.
(function (root) {
  "use strict";

  /**
   * Wires one link-chip picker's click behaviour under `container`
   * (clicking a live search result adds a chip; clicking a chip's own
   * remove button removes it). Idempotent per element: re-running this on
   * the same still-attached `container` is a no-op, so a caller does not
   * need to track whether it already wired a given fragment.
   */
  function wireLinkChips(container) {
    if (!container || container.dataset.linkChipsWired === "true") {
      return;
    }
    container.dataset.linkChipsWired = "true";
    container.addEventListener("click", function (ev) {
      var removeButton = ev.target.closest(".link-chip-remove");
      if (removeButton) {
        var chip = removeButton.closest(".link-chip");
        if (chip) {
          chip.remove();
        }
        return;
      }
      var result = ev.target.closest(".link-search-result");
      if (!result) {
        return;
      }
      ev.preventDefault();
      var field = container.querySelector('.task-link-field[data-field="' + result.dataset.field + '"]');
      if (!field) {
        return;
      }
      var list = field.querySelector(".link-chip-list");
      var target = result.dataset.target;
      var single = field.dataset.single === "true";
      var already = Array.prototype.slice.call(list.querySelectorAll(".link-chip")).some(function (c) {
        return c.dataset.target === target;
      });
      if (single) {
        list.innerHTML = "";
      }
      if (single || !already) {
        var newChip = document.createElement("span");
        newChip.className = "link-chip";
        newChip.dataset.target = target;
        var hidden = document.createElement("input");
        hidden.type = "hidden";
        hidden.name = result.dataset.field;
        hidden.value = target;
        newChip.appendChild(hidden);
        var idSpan = document.createElement("span");
        idSpan.className = "mono";
        idSpan.textContent = result.dataset.display;
        newChip.appendChild(idSpan);
        var titleSpan = document.createElement("span");
        titleSpan.className = "link-chip-title";
        titleSpan.textContent = result.dataset.title || "";
        newChip.appendChild(titleSpan);
        var removeBtn = document.createElement("button");
        removeBtn.type = "button";
        removeBtn.className = "link-chip-remove";
        removeBtn.setAttribute("aria-label", "Remove link");
        removeBtn.textContent = "×";
        newChip.appendChild(removeBtn);
        list.appendChild(newChip);
      }
      var search = field.querySelector(".link-search");
      var input = search.querySelector(".link-search-input");
      var results = search.querySelector(".link-search-results");
      if (input) {
        input.value = "";
      }
      if (results) {
        results.innerHTML = "";
      }
    });
  }

  root.contextosTasks = { wireLinkChips: wireLinkChips };
})(typeof window !== "undefined" ? window : globalThis);
