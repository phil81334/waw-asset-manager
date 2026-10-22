import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { evaluate } from "./conditions.js";

const element = (id) => document.getElementById(id);
const roles = [
  "colorMap",
  "normalMap",
  "detailMap",
  "specColorMap",
  "cosinePowerMap",
];
const roleNames = [
  "Colour",
  "Normal",
  "Detail normal",
  "Specular colour",
  "Cosine / gloss",
];
const state = {
  schemas: [],
  files: [],
  document: null,
  selected: null,
  dirty: false,
  busy: false,
  images: [],
};
let nextId = 0;
let tab = "assets";
let logText = "";
let preferences = {};

function node(tag, text, className) {
  const result = document.createElement(tag);
  if (text !== undefined) {
    result.textContent = text;
  }
  if (className) {
    result.className = className;
  }
  return result;
}

function message(text, error = false) {
  element("message").textContent = String(text);
  element("message").className = error ? "error" : "success";
  element("status").textContent = error ? "Error" : "Done";
}

function appendLog(line) {
  logText += `${line}\n`;
  element("log").textContent = logText.slice(-250000);
  element("log").scrollTop = element("log").scrollHeight;
}

function lock(locked) {
  state.busy = locked;
  for (const control of element("app").querySelectorAll(
    "button, input, select, textarea",
  )) {
    control.disabled = locked;
  }
  element("cancel").disabled = !locked;
  element("copy-log").disabled = false;
  element("clear-log").disabled = locked;
  element("width").disabled = locked || element("resize").value !== "exact";
  element("height").disabled = locked || element("resize").value !== "exact";
}

async function work(action) {
  if (state.busy) {
    return;
  }
  lock(true);
  element("cancel").disabled = true;
  element("status").textContent = "Working…";
  try {
    await action();
  } catch (error) {
    appendLog(String(error));
    message(error, true);
  } finally {
    lock(false);
    if (element("status").textContent === "Working…") {
      element("status").textContent = "Ready";
    }
  }
}

function ask(title, text, fields = []) {
  const dialog = element("action-dialog");
  element("action-title").textContent = title;
  element("action-message").textContent = text;
  element("action-fields").replaceChildren();
  const inputs = fields.map((field) => {
    const label = node("label", field.label);
    const input = node(field.options ? "select" : "input");
    if (field.options) {
      for (const [value, caption] of field.options) {
        input.add(new Option(caption, value));
      }
    }
    input.value = field.value || "";
    input.required = true;
    label.append(input);
    element("action-fields").append(label);
    return input;
  });
  return new Promise((resolve) => {
    let finished = false;
    function finish(result) {
      if (finished) {
        return;
      }
      finished = true;
      dialog.close();
      resolve(result);
    }
    element("action-form").onsubmit = (event) => {
      event.preventDefault();
      finish(inputs.map((input) => input.value));
    };
    element("action-cancel").onclick = () => finish(null);
    dialog.oncancel = (event) => {
      event.preventDefault();
      finish(null);
    };
    dialog.showModal();
    (inputs[0] || element("action-ok")).focus();
    if (inputs[0]?.select) {
      inputs[0].select();
    }
  });
}

async function discard() {
  return (
    !state.dirty ||
    (await ask(
      "Discard unsaved edits?",
      "Your GDT edits have not been saved. Continue without saving them?",
    )) !== null
  );
}

function changed() {
  state.dirty = true;
  updatePath();
}

function updatePath() {
  element("document-path").textContent = state.document
    ? `${state.document.path}${state.dirty ? " — unsaved edits" : ""}`
    : "Open a GDT or create a new one.";
}

function currentEntry() {
  return state.document?.entries.find((entry) => entry.id === state.selected);
}

function currentSchema(entry = currentEntry()) {
  return state.schemas.find((schema) => schema.file === entry?.schema);
}

function setDocument(document) {
  state.document = document;
  state.selected = document.entries[0]?.id ?? null;
  nextId = Math.max(-1, ...document.entries.map((entry) => entry.id)) + 1;
  state.dirty = false;
  element("external-change").hidden = true;
  element("gdt-files").value = document.path;
  updatePath();
  renderAssets();
  renderProperties();
}

function renderFiles() {
  const select = element("gdt-files");
  select.replaceChildren(new Option("Choose a GDT…", ""));
  for (const file of state.files) {
    select.add(new Option(file.path, file.path));
  }
  if (
    state.document &&
    !state.files.some((file) => file.path === state.document.path)
  ) {
    select.add(new Option(state.document.path, state.document.path));
  }
  select.value = state.document?.path || "";
}

function renderAssets() {
  const search = element("asset-search").value.toLowerCase();
  const type = element("type-filter").value;
  const entries = state.document?.entries || [];
  const visible = entries.filter(
    (entry) =>
      entry.name.toLowerCase().includes(search) &&
      (!type || entry.schema === type),
  );
  const list = element("asset-list");
  list.replaceChildren();
  for (const entry of visible) {
    const item = node(
      "button",
      entry.name,
      `asset${entry.id === state.selected ? " selected" : ""}`,
    );
    item.setAttribute("role", "option");
    item.setAttribute("aria-selected", String(entry.id === state.selected));
    item.title = entry.name;
    item.append(node("small", entry.schema.replace(".gdf", "")));
    item.onclick = () => {
      state.selected = entry.id;
      renderAssets();
      renderProperties();
    };
    list.append(item);
  }
  element("asset-count").textContent =
    `${visible.length} of ${entries.length} assets`;
}

function isImageProperty(entry, field) {
  if (!["material.gdf", "light.gdf"].includes(entry.schema)) {
    return false;
  }
  // Use installed image file controls, including when All fields is selected.
  function containsImage(controls) {
    return controls.some((control) => {
      return (
        (control.kind === "fileedit" && control.binding === field.name) ||
        containsImage(control.children)
      );
    });
  }
  return containsImage(currentSchema(entry)?.controls || []);
}

async function preparePropertyPath(entry, field, path) {
  if (!isImageProperty(entry, field) || !/\.png$/i.test(path)) {
    return path;
  }
  const prepared = await invoke("prepare_property_image", { path });
  const text = `PNG prepared without resizing: ${prepared}`;
  appendLog(text);
  message(text);
  return prepared;
}

function fieldRow(entry, field, control, values) {
  const labelText = control?.label || field.name;
  const search = element("property-search").value.toLowerCase();
  if (search && !`${field.name} ${labelText}`.toLowerCase().includes(search)) {
    return null;
  }
  const row = node("div", undefined, "property");
  if (control?.kind.includes("fileedit") || control?.kind === "multiedit") {
    row.classList.add("wide-property");
  }
  const label = node("label", labelText);
  label.title = `${field.name}\n${control?.tooltip || ""}\nDefault: ${field.default}`;
  const holder = node("div", undefined, "value");
  const previousValue = entry.properties[field.name];
  const value = previousValue ?? field.default;
  let input;
  if (field.kind === "enum" || field.kind === "keyenum") {
    input = node("select");
    const choices = new Set(field.choices);
    if (field.kind === "keyenum") {
      choices.add("");
      for (const asset of state.document.entries) {
        if (field.references.includes(asset.schema)) {
          choices.add(asset.name);
        }
      }
      // Referenced assets from other GDTs are added by refreshReferenceChoices.
      for (const asset of referenceEntries) {
        if (field.references.includes(asset.schema)) {
          choices.add(asset.name);
        }
      }
    }
    choices.add(value);
    for (const choice of choices) {
      input.add(new Option(choice || "(not set)", choice));
    }
    input.value = value;
  } else {
    input = node(control?.kind === "multiedit" ? "textarea" : "input");
    if (control?.kind === "checkbox") {
      input.type = "checkbox";
      input.checked = Number(value) !== 0;
    } else if (["int", "float"].includes(field.kind)) {
      input.type = "number";
      input.step = field.kind === "int" ? "1" : "any";
      input.min = field.minimum ?? "";
      input.max = field.maximum ?? "";
      input.value = value;
    } else {
      input.value = value;
    }
  }
  input.id = `property-${field.name}`;
  label.htmlFor = input.id;
  input.dataset.field = field.name;
  const update = () => {
    entry.properties[field.name] =
      input.type === "checkbox" ? String(Number(input.checked)) : input.value;
    changed();
  };
  input.addEventListener("input", update);
  input.addEventListener("change", () => {
    if (isImageProperty(entry, field) && /\.png$/i.test(input.value)) {
      work(async () => {
        try {
          entry.properties[field.name] = await preparePropertyPath(
            entry,
            field,
            input.value,
          );
          changed();
        } catch (error) {
          // Keep an invalid/unsupported PNG from replacing the previous path.
          if (previousValue === undefined) {
            delete entry.properties[field.name];
          } else {
            entry.properties[field.name] = previousValue;
          }
          throw error;
        } finally {
          renderProperties();
        }
      });
      return;
    }
    update();
    renderProperties();
  });
  holder.append(input);
  if (control?.kind.includes("fileedit")) {
    const browse = node("button", "…");
    browse.title = `Choose ${labelText}`;
    browse.onclick = () =>
      work(async () => {
        const result = await invoke("pick_property", {
          directory: control.directory || "",
          multiple: control.kind === "multifileedit",
          image: isImageProperty(entry, field),
        });
        if (result !== null) {
          entry.properties[field.name] = await preparePropertyPath(
            entry,
            field,
            result,
          );
          changed();
          renderProperties();
        }
      });
    holder.append(browse);
  }
  const reset = node("button", "↺", "reset-field");
  reset.title = "Use the schema default";
  reset.onclick = () => {
    delete entry.properties[field.name];
    changed();
    renderProperties();
  };
  row.append(label, holder, reset);
  return row;
}

function arrangePropertyColumns(target) {
  // Visibility is resolved before arranging rows. Unnamed native containers
  // organise controls, but only named sections need a visible boundary here.
  const containers = [...target.querySelectorAll(".property-container")];
  for (const container of containers.reverse()) {
    container.replaceWith(...container.childNodes);
  }
  for (const section of [target, ...target.querySelectorAll("fieldset")]) {
    let grid = null;
    for (const child of [...section.children]) {
      if (child.classList.contains("property")) {
        if (!grid) {
          grid = node("div", undefined, "property-grid");
          child.before(grid);
        }
        grid.append(child);
      } else {
        grid = null;
      }
    }
  }
}

function renderProperties() {
  const target = element("properties");
  target.dataset.layout = element("property-layout").value;
  target.replaceChildren();
  const entry = currentEntry();
  element("asset-title").textContent = entry?.name || "Select an asset";
  element("asset-type").textContent = entry?.schema.replace(".gdf", "") || "";
  if (!entry) {
    return;
  }
  const schema = currentSchema(entry);
  if (!schema) {
    target.append(
      node(
        "p",
        "This schema is not installed. Stored fields are preserved.",
        "warning",
      ),
    );
  }
  const fields = schema?.fields || [];
  const values = Object.fromEntries(
    fields.map((field) => [field.name, field.default]),
  );
  Object.assign(values, entry.properties);
  const shown = new Set();
  const all = element("all-fields").checked;
  function appendControl(control, parent) {
    if (control.kind === "fxvcontainer") {
      return;
    }
    const visible = evaluate(control.visible, values);
    if (!visible || visible === "0") {
      return;
    }
    if (control.children.length) {
      const group = node(control.label ? "fieldset" : "div");
      if (!control.label) {
        group.classList.add("property-container");
      }
      if (control.label) {
        group.append(node("legend", control.label));
      }
      for (const child of control.children) {
        appendControl(child, group);
      }
      if (group.querySelector(".property")) {
        parent.append(group);
      }
      return;
    }
    let binding = control.binding;
    if (binding.includes("?")) {
      const parts = binding.match(/^(.*?)\?\s*(\w+)\s*:\s*(\w+)$/);
      if (!parts) {
        throw new Error("Unsupported conditional field binding");
      }
      binding = evaluate(parts[1], values) ? parts[2] : parts[3];
    }
    const field = fields.find((field) => field.name === binding);
    if (field && !shown.has(field.name)) {
      const row = fieldRow(entry, field, control, values);
      if (row) {
        parent.append(row);
      }
      shown.add(field.name);
    }
  }
  try {
    if (!all) {
      for (const control of schema?.controls || []) {
        appendControl(control, target);
      }
    } else {
      for (const field of fields) {
        const row = fieldRow(entry, field, null, values);
        if (row) {
          target.append(row);
        }
      }
    }
    const unknown = Object.keys(entry.properties).filter(
      (key) => !fields.some((field) => field.name === key),
    );
    if (unknown.length) {
      const group = node("fieldset");
      group.append(node("legend", "Other stored fields"));
      for (const name of unknown) {
        const field = { name, kind: "string", default: "" };
        const row = fieldRow(entry, field, null, values);
        if (row) {
          group.append(row);
        }
      }
      target.append(group);
    }
    if (entry.schema === "material.gdf") {
      target.append(
        node(
          "p",
          "CGFX-specific controls are not included. Existing CGFX values are kept when saving.",
          "hint",
        ),
      );
    }
  } catch (error) {
    target.append(
      node(
        "p",
        `${error}. Use All fields to inspect ordinary properties.`,
        "warning",
      ),
    );
  }
  if (target.dataset.layout === "responsive") {
    arrangePropertyColumns(target);
  }
  if (state.busy) {
    for (const input of target.querySelectorAll(
      "input, button, select, textarea",
    )) {
      input.disabled = true;
    }
  }
}

let referenceEntries = [];
async function refreshReferenceChoices() {
  referenceEntries = await invoke("reference_entries");
}

async function refreshFiles() {
  state.files = await invoke("list_files");
  renderFiles();
  if (state.document?.fingerprint) {
    const disk = state.files.find((file) => file.path === state.document.path);
    element("external-change").hidden =
      disk?.fingerprint === state.document.fingerprint;
  }
}

async function save() {
  if (!state.document) {
    throw new Error("Open or create a GDT first.");
  }
  const selected = state.selected;
  const saved = await invoke("save_gdt", { document: state.document });
  const selectedName = currentEntry()?.name;
  setDocument(saved);
  state.selected =
    saved.entries.find((entry) => entry.name === selectedName)?.id ?? selected;
  await refreshFiles();
  await refreshReferenceChoices();
  renderAssets();
  renderProperties();
  message("GDT saved. Original images are unchanged.");
}

async function convert(all) {
  await save();
  const ids = all
    ? state.document.entries.map((entry) => entry.id)
    : [state.selected];
  if (!ids.length || ids[0] === null) {
    throw new Error("Select an asset first.");
  }
  if (
    (await ask(
      "Convert assets?",
      `Convert ${ids.length} asset(s) using the installed PC converter?`,
    )) === null
  ) {
    return;
  }
  element("log-panel").open = true;
  lock(true);
  element("cancel").disabled = false;
  element("status").textContent = "Converting…";
  message(await invoke("convert", { document: state.document, ids }));
}

function showTab(name) {
  tab = name;
  element("assets-panel").hidden = name !== "assets";
  element("images-panel").hidden = name !== "images";
  for (const button of document.querySelectorAll(".tab")) {
    const active = button.dataset.tab === name;
    button.classList.toggle("active", active);
    button.setAttribute("aria-selected", String(active));
  }
}

function assignRule(image) {
  const stem = image.filename.replace(/\.[^.]+$/, "");
  const matches = [];
  for (const role of roles) {
    const template = element(`rule-${role}`).value;
    const parts = template.split("{name}");
    if (
      parts.length !== 2 ||
      !stem.startsWith(parts[0]) ||
      !stem.endsWith(parts[1])
    ) {
      continue;
    }
    const name = stem.slice(parts[0].length, stem.length - parts[1].length);
    if (name) {
      matches.push({ role, name });
    }
  }
  image.role = matches.length === 1 ? matches[0].role : "";
  image.name = matches.length === 1 ? matches[0].name : stem;
}

function renderImages() {
  const list = element("image-list");
  list.replaceChildren();
  if (!state.images.length) {
    list.append(
      node(
        "p",
        "Images are prepared as TGA copies. Your original files are kept.",
        "empty",
      ),
    );
  }
  for (const image of state.images) {
    const row = node("div", undefined, "image-row");
    const thumb = node("img");
    thumb.src = image.thumbnail;
    thumb.alt = image.filename;
    const caption = node("div", image.filename, "filename");
    caption.title = image.path;
    caption.append(node("p", `${image.width} × ${image.height}`, "hint"));
    const name = node("input");
    name.value = image.name;
    name.setAttribute("aria-label", `Material name for ${image.filename}`);
    name.oninput = () => {
      image.name = name.value;
    };
    const role = node("select");
    role.setAttribute("aria-label", `Texture role for ${image.filename}`);
    role.add(new Option("Choose role…", ""));
    roles.forEach((value, index) =>
      role.add(new Option(roleNames[index], value)),
    );
    role.value = image.role;
    role.onchange = () => {
      image.role = role.value;
    };
    const remove = node("button", "×");
    remove.title = `Remove ${image.filename}`;
    remove.onclick = () => {
      state.images = state.images.filter((item) => item !== image);
      renderImages();
    };
    row.append(thumb, caption, name, role, remove);
    list.append(row);
  }
  updateResize();
}

async function addImages(paths) {
  const images = await invoke("import_images", { paths: paths || null });
  for (const image of images) {
    if (!state.images.some((item) => item.path === image.path)) {
      assignRule(image);
      state.images.push(image);
    }
  }
  showTab("images");
  renderImages();
  message(
    `${state.images.length} image(s) added. Review names and roles before preparing.`,
  );
}

function dimensions() {
  if (element("resize").value === "keep") {
    return { width: null, height: null };
  }
  if (element("resize").value === "fit") {
    if (!state.images.length) {
      return { width: null, height: null };
    }
    const smallest = Math.min(
      ...state.images.flatMap((image) => [image.width, image.height]),
    );
    const side = 2 ** Math.floor(Math.log2(smallest));
    return { width: side, height: side };
  }
  return {
    width: Number(element("width").value),
    height: Number(element("height").value),
  };
}

function updateResize() {
  const exact = element("resize").value === "exact";
  element("width").disabled = !exact;
  element("height").disabled = !exact;
  const size = dimensions();
  element("resize-info").textContent = size.width
    ? `Target: ${size.width} × ${size.height}. No upscaling. Aspect-ratio changes require confirmation.`
    : "No resizing. Transparency is kept. No upscaling, automatic cropping or padding.";
}

async function prepare() {
  if (!state.document) {
    throw new Error("Create or open a GDT in Assets first.");
  }
  if (!state.images.length) {
    throw new Error("Add images first.");
  }
  const names = new Set();
  for (const image of state.images) {
    if (!image.role || !/^[A-Za-z0-9_]+$/.test(image.name)) {
      throw new Error(
        "Assign every image a role and a material name using letters, numbers or underscores.",
      );
    }
    if (
      state.document.entries.some(
        (entry) =>
          entry.schema === "material.gdf" &&
          entry.name.toLowerCase() === image.name.toLowerCase(),
      )
    ) {
      throw new Error(
        `${image.name} already exists in this GDT. Choose a new material name.`,
      );
    }
    names.add(image.name);
  }
  const size = dimensions();
  if (
    size.width !== null &&
    (!Number.isInteger(size.width) ||
      !Number.isInteger(size.height) ||
      size.width <= 0 ||
      size.height <= 0)
  ) {
    throw new Error("Choose positive whole-number dimensions.");
  }
  const changesAspect =
    size.width &&
    state.images.some(
      (image) => image.width * size.height !== image.height * size.width,
    );
  const text = `Prepare ${state.images.length} image(s) for ${names.size} material(s)? Copies go in texture_assets/${element("project").value}. ${changesAspect ? "The selected dimensions change image proportions." : "Original files are kept."}`;
  if ((await ask("Prepare images?", text)) === null) {
    return;
  }
  const entries = await invoke("prepare_images", {
    request: {
      project: element("project").value,
      material_type: element("material-type").value,
      assignments: state.images.map(({ path, name, role }) => ({
        path,
        name,
        role,
      })),
      ...size,
    },
  });
  for (const entry of entries) {
    entry.id = nextId++;
    state.document.entries.push(entry);
  }
  state.selected = entries[0]?.id ?? state.selected;
  changed();
  showTab("assets");
  renderAssets();
  renderProperties();
  message(
    `Prepared ${state.images.length} image(s); added ${entries.length} material(s). Save the GDT, then convert.`,
  );
}

for (const button of document.querySelectorAll(".tab")) {
  button.onclick = () => showTab(button.dataset.tab);
}
element("asset-search").oninput = renderAssets;
element("type-filter").onchange = renderAssets;
element("property-search").oninput = renderProperties;
element("all-fields").onchange = renderProperties;
element("property-layout").onchange = () => {
  renderProperties();
  if (isTauri()) {
    work(async () => {
      const value = {
        ...preferences,
        propertyLayout: element("property-layout").value,
      };
      await invoke("settings", { value });
      preferences = value;
    });
  }
};
element("save").onclick = () => work(save);
element("convert-selected").onclick = () => work(() => convert(false));
element("convert-file").onclick = () => work(() => convert(true));
element("cancel").onclick = async () => {
  element("cancel").disabled = true;
  element("status").textContent = "Cancelling…";
  try {
    await invoke("cancel");
  } catch (error) {
    element("cancel").disabled = false;
    message(`Could not cancel: ${error}`, true);
  }
};
element("rescan").onclick = () =>
  work(async () => {
    await refreshFiles();
    await refreshReferenceChoices();
    message("GDT list refreshed. Unsaved edits have been kept.");
  });
element("open").onclick = () =>
  work(async () => {
    if (await discard()) {
      const document = await invoke("open_gdt", { path: null });
      if (document) {
        setDocument(document);
      }
    }
  });
element("gdt-files").onchange = () =>
  work(async () => {
    const path = element("gdt-files").value;
    if (path && (await discard())) {
      setDocument(await invoke("open_gdt", { path }));
    }
    renderFiles();
  });
element("reload").onclick = () =>
  work(async () => {
    if (await discard()) {
      setDocument(await invoke("open_gdt", { path: state.document.path }));
    }
  });
element("new-gdt").onclick = () =>
  work(async () => {
    if (!(await discard())) {
      return;
    }
    const answer = await ask(
      "New GDT",
      "Create a new table in a Modtools source folder.",
      [{ label: "GDT path", value: "source_data/my_assets.gdt" }],
    );
    if (answer) {
      setDocument({
        path: answer[0].replaceAll("\\", "/"),
        fingerprint: "",
        entries: [],
      });
      changed();
      renderFiles();
    }
  });
async function assetAction(action) {
  if (!state.document) {
    throw new Error("Open or create a GDT first.");
  }
  const current = currentEntry();
  if (action !== "new" && !current) {
    throw new Error("Select an asset first.");
  }
  if (action === "delete") {
    if (
      (await ask(
        "Delete asset?",
        `Remove ${current.name} from this GDT? Compiled output is not deleted.`,
      )) !== null
    ) {
      state.document.entries = state.document.entries.filter(
        (entry) => entry !== current,
      );
      state.selected = state.document.entries[0]?.id ?? null;
      changed();
    }
  } else {
    const fields = [
      {
        label: "Asset name",
        value:
          action === "new"
            ? "my_asset"
            : `${current.name}${action === "copy" ? "_copy" : ""}`,
      },
    ];
    if (action === "new") {
      fields.push({
        label: "Asset type",
        value: "material.gdf",
        options: state.schemas.map((schema) => [schema.file, schema.name]),
      });
    }
    const answer = await ask(
      `${action[0].toUpperCase()}${action.slice(1)} asset`,
      "Use a unique asset name. Conversion selects by type and name across all GDT files.",
      fields,
    );
    if (!answer) {
      return;
    }
    const schema = answer[1] || current.schema;
    if (
      state.document.entries.some(
        (entry) =>
          entry !== (action === "rename" ? current : null) &&
          entry.schema === schema &&
          entry.name.toLowerCase() === answer[0].toLowerCase(),
      )
    ) {
      throw new Error("That asset name already exists in this GDT.");
    }
    if (action === "rename") {
      current.name = answer[0];
    } else {
      const entry = {
        id: nextId++,
        name: answer[0],
        schema,
        properties: action === "copy" ? { ...current.properties } : {},
      };
      state.document.entries.push(entry);
      state.selected = entry.id;
    }
    changed();
  }
  renderAssets();
  renderProperties();
}
for (const action of ["new", "copy", "rename", "delete"]) {
  element(`${action}-asset`).onclick = () => work(() => assetAction(action));
}
element("add-images").onclick = () => work(() => addImages());
element("clear-images").onclick = () => {
  state.images = [];
  renderImages();
};
element("prepare").onclick = () => work(prepare);
element("resize").onchange = updateResize;
element("width").oninput = updateResize;
element("height").oninput = updateResize;
element("apply-rules").onclick = () => {
  state.images.forEach(assignRule);
  renderImages();
};
element("save-rules").onclick = () =>
  work(async () => {
    const value = Object.fromEntries(
      roles.map((role) => [role, element(`rule-${role}`).value]),
    );
    for (const template of Object.values(value)) {
      if (template && template.split("{name}").length !== 2) {
        throw new Error(
          "Each enabled template must contain {name} exactly once.",
        );
      }
    }
    const updated = { ...preferences, ...value };
    await invoke("settings", { value: updated });
    preferences = updated;
    message("Naming rules saved locally.");
  });
element("copy-log").onclick = async () => {
  try {
    await navigator.clipboard.writeText(logText);
    if (!state.busy) {
      message("Log copied.");
    }
  } catch (error) {
    message(`Could not copy the log: ${error}`, true);
  }
};
element("clear-log").onclick = () => {
  logText = "";
  element("log").textContent = "";
};
element("about").onclick = () => {
  element("about-dialog").showModal();
  element("repo-url").focus();
  element("repo-url").select();
};
element("repo-url").onclick = () => element("repo-url").select();
element("close-about").onclick = () => element("about-dialog").close();
element("startup-error").oncancel = (event) => event.preventDefault();
element("quit").onclick = () => invoke("quit");

async function start() {
  element("status").textContent = "Loading…";
  element("environment").textContent = import.meta.env.DEV
    ? "Development"
    : "Production";
  const defaults = [
    "{name}_c",
    "{name}_n",
    "{name}_detail",
    "{name}_s",
    "{name}_g",
  ];
  roles.forEach((role, index) => {
    const label = node("label", roleNames[index]);
    const input = node("input");
    input.id = `rule-${role}`;
    input.value = defaults[index];
    label.append(input);
    element("rules").append(label);
  });
  if (!isTauri()) {
    element("app").hidden = false;
    message(
      "Browser preview only. Use npm run dev to open GDTs, import images and convert assets.",
    );
    for (const schema of ["world phong", "model phong", "2d"]) {
      element("material-type").add(new Option(schema, schema));
    }
    return;
  }
  try {
    const info = await invoke("startup");
    state.schemas = info.schemas;
    state.files = info.files;
    element("log-location").textContent =
      `Last conversion log: ${info.cache}\\last-conversion.txt`;
    for (const schema of state.schemas) {
      element("type-filter").add(new Option(schema.name, schema.file));
    }
    for (const choice of state.schemas
      .find((schema) => schema.file === "material.gdf")
      .fields.find((field) => field.name === "materialType").choices) {
      if (!choice.startsWith("CGFX")) {
        element("material-type").add(new Option(choice, choice));
      }
    }
    renderFiles();
    await refreshReferenceChoices();
    const rules = await invoke("settings", { value: null });
    if (rules) {
      preferences = rules;
      if (["single", "responsive"].includes(rules.propertyLayout)) {
        element("property-layout").value = rules.propertyLayout;
      }
      for (const role of roles) {
        if (typeof rules[role] === "string") {
          element(`rule-${role}`).value = rules[role];
        }
      }
    }
    await listen("conversion-log", (event) => appendLog(event.payload));
    await getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "drop" && !state.busy) {
        work(() => addImages(event.payload.paths));
      }
    });
    await getCurrentWindow().onCloseRequested(async (event) => {
      if (state.busy) {
        event.preventDefault();
        message(
          "Cancel the conversion and wait for it to stop before closing.",
          true,
        );
      } else if (state.dirty) {
        event.preventDefault();
        if (await discard()) {
          await invoke("quit");
        }
      }
    });
    element("app").hidden = false;
    message(
      `${state.schemas.length} installed asset schemas loaded. Open a GDT to begin.`,
    );
    element("status").textContent = "Ready";
    setInterval(async () => {
      if (!state.busy && !element("action-dialog").open) {
        try {
          await refreshFiles();
        } catch (error) {
          appendLog(`Refresh: ${error}`);
        }
      }
    }, 5000);
  } catch (error) {
    const text = String(error);
    if (text.includes("Install the World at War Modtools")) {
      element("startup-title").textContent = "World at War Modtools required";
    } else if (!text.includes("beside CoDWaW.exe")) {
      element("startup-title").textContent = "Could not open Asset Manager";
    }
    element("startup-message").textContent = String(error);
    element("startup-error").showModal();
  }
}

start();

document.addEventListener("keydown", (event) => {
  if (event.ctrlKey && event.key.toLowerCase() === "s") {
    event.preventDefault();
    if (!state.busy && !document.querySelector("dialog[open]")) {
      work(save);
    }
  }
});
