//! Block attribute extraction for Open CAD Studio.
//!
//! With an active selection, only the selected INSERTs are processed.
//!
//! BA_LIST   - print every INSERT's attributes to the command line.
//! BA_UPDATE - update existing INSERTs from a CSV (file picker, or
//!             `BA_UPDATE <path>`). Rows match by Handle, else by Block+X/Y/Z.
//! BA_CREATE - create new INSERTs from a CSV (Block,X,Y,Z + one column per tag).
//! BA_EXPORT - write a CSV (one row per INSERT: handle, block, layer,
//!             insertion point X/Y/Z, then one column per tag).
//!             `BA_EXPORT <path>` writes there; bare `BA_EXPORT` writes
//!             <drawing>.attributes.csv next to the saved drawing.

use ocs_plugin_api::export_plugin;
use ocs_plugin_api::host::document::entities::{AttributeEntity, Insert};
use ocs_plugin_api::host::document::types::Vector3;
use ocs_plugin_api::host::{
    BuiltinPlugin, CommandStep, EntityType, HostApi, InteractiveCommand, LayerConfig,
};
use ocs_plugin_api::dialog::{DialogControl, DialogDef, DialogResponse, DialogValue};
use ocs_plugin_api::manifest::{ApiVersion, PluginManifest};
use ocs_plugin_api::ribbon::{CadModule, IconKind, ModuleEvent, RibbonGroup, RibbonItem, ToolDef};
use std::collections::HashSet;
use std::sync::Mutex;
use std::path::PathBuf;

static MANIFEST: PluginManifest = PluginManifest {
    id: "opencad.block_attributes",
    name: "Block Attributes",
    version: env!("CARGO_PKG_VERSION"),
    description: "Extract block attribute data to CSV.",
    api_version: ApiVersion::CURRENT,
    ribbon_order: 60,
    xdata_apps: &[],
    command_prefixes: &["BA_EXPORT", "BA_LIST", "BA_UPDATE", "BA_CREATE", "BA_DEBUG"],
};

struct Row {
    handle: String,
    block: String,
    layer: String,
    x: f64,
    y: f64,
    z: f64,
    attrs: Vec<(String, String)>, // (tag, value) in file order
}

/// Read INSERTs and their attributes from the cached document copy.
/// If a selection is active, only selected INSERTs are returned;
/// with no selection, every INSERT is. The bool is true when filtered.
fn collect(host: &dyn HostApi) -> (Vec<Row>, bool) {
    let selected: HashSet<_> = host.selection().into_iter().collect();
    let filtered = !selected.is_empty();
    let rows = host
        .document()
        .entities()
        .filter_map(|e| match e {
            EntityType::Insert(ins) if filtered && !selected.contains(&ins.common.handle) => None,
            EntityType::Insert(ins) => Some(Row {
                handle: ins.common.handle.to_string(), // ASSUMED field path
                block: ins.block_name.clone(),
                layer: ins.common.layer.clone(),
                x: ins.insert_point.x,
                y: ins.insert_point.y,
                z: ins.insert_point.z,
                attrs: ins
                    .attributes
                    .iter()
                    .map(|a| (a.tag.clone(), a.value.clone())) // ASSUMED: `value`
                    .collect(),
            }),
            _ => None,
        })
        .collect();
    (rows, filtered)
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn to_csv(rows: &[Row]) -> String {
    // Columns = union of tags, first-seen order.
    let mut tags: Vec<&str> = Vec::new();
    for r in rows {
        for (t, _) in &r.attrs {
            if !tags.contains(&t.as_str()) {
                tags.push(t);
            }
        }
    }
    let mut out = String::from("Handle,Block,Layer,X,Y,Z");
    for t in &tags {
        out.push(',');
        out.push_str(&csv_field(t));
    }
    out.push('\n');
    for r in rows {
        out.push_str(&format!(
            "{},{},{},{},{},{}",
            csv_field(&r.handle),
            csv_field(&r.block),
            csv_field(&r.layer),
            r.x,
            r.y,
            r.z
        ));
        for t in &tags {
            let v = r.attrs.iter().find(|(k, _)| k == t).map(|(_, v)| v.as_str());
            out.push(',');
            out.push_str(&csv_field(v.unwrap_or("")));
        }
        out.push('\n');
    }
    out
}

// ── CSV import helpers ──────────────────────────────────────────────────────

/// Minimal RFC-4180 parser: quotes, doubled quotes, embedded newlines, CRLF, BOM.
fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_q = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_q {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_q = false;
                }
            } else {
                field.push(c);
            }
        } else {
            match c {
                '"' => in_q = true,
                ',' => row.push(std::mem::take(&mut field)),
                '\r' => {}
                '\n' => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                _ => field.push(c),
            }
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    // Drop fully blank lines (e.g. trailing newline).
    rows.retain(|r| r.iter().any(|f| !f.trim().is_empty()));
    rows
}

/// Parsed CSV: well-known columns located by (case-insensitive) header name,
/// every other column is an attribute tag.
struct Table {
    handle: Option<usize>,
    block: Option<usize>,
    layer: Option<usize>,
    x: Option<usize>,
    y: Option<usize>,
    z: Option<usize>,
    tags: Vec<(String, usize)>, // (tag, column)
    rows: Vec<Vec<String>>,
}

impl Table {
    fn parse(text: &str) -> Result<Table, String> {
        let mut rows = parse_csv(text);
        if rows.is_empty() {
            return Err("CSV is empty".into());
        }
        let header = rows.remove(0);
        let find = |name: &str| header.iter().position(|h| h.trim().eq_ignore_ascii_case(name));
        let (handle, block, x, y, z) =
            (find("handle"), find("block"), find("x"), find("y"), find("z"));
        let layer = find("layer");
        let known = [handle, block, x, y, z, layer];
        let tags = header
            .iter()
            .enumerate()
            .filter(|(i, h)| !known.contains(&Some(*i)) && !h.trim().is_empty())
            .map(|(i, h)| (h.trim().to_string(), i))
            .collect();
        Ok(Table { handle, block, layer, x, y, z, tags, rows })
    }

    fn cell<'a>(&self, row: &'a [String], col: Option<usize>) -> &'a str {
        col.and_then(|c| row.get(c)).map(|s| s.trim()).unwrap_or("")
    }

    fn num(&self, row: &[String], col: Option<usize>) -> Option<f64> {
        self.cell(row, col).parse::<f64>().ok()
    }

    fn xyz(&self, row: &[String]) -> Option<(f64, f64, f64)> {
        Some((
            self.num(row, self.x)?,
            self.num(row, self.y)?,
            self.num(row, self.z).unwrap_or(0.0),
        ))
    }

    /// Value for `tag` in `row`: exact header match first, then case-insensitive.
    fn tag_value<'a>(&self, row: &'a [String], tag: &str) -> Option<&'a str> {
        let col = self
            .tags
            .iter()
            .find(|(t, _)| t == tag)
            .or_else(|| self.tags.iter().find(|(t, _)| t.eq_ignore_ascii_case(tag)))?
            .1;
        Some(row.get(col).map(|s| s.as_str()).unwrap_or(""))
    }
}

/// Path chosen in the file dialog, handed to the re-dispatched command
/// (kept out of the command line so case/spaces in the path survive).
static PENDING_CSV: Mutex<Option<PathBuf>> = Mutex::new(None);
const PENDING_MARK: &str = "@";

/// Interactive command whose only job is to show the file-picker dialog and,
/// once answered, re-dispatch `<cmd> @`. (HostApi::show_dialog is fire-and-forget
/// in the host: it opens the dialog and returns Cancelled at once.)
struct PickCsv {
    cmd: &'static str,
    title: &'static str,
    default_path: String,
}

impl InteractiveCommand for PickCsv {
    fn prompt(&self) -> String {
        format!("{}: choose the source CSV in the dialog", self.cmd)
    }
    fn on_point(&mut self, _pt: [f64; 3]) -> CommandStep {
        CommandStep::Cancel
    }
    fn modal_dialog(&self) -> Option<DialogDef> {
        Some(
            DialogDef::new(self.title)
                .submit_label("Run")
                .with_control(DialogControl::FilePicker {
                    id: "src".into(),
                    label: "Source CSV:".into(),
                    default_path: self.default_path.clone(),
                    filter_name: "CSV files".into(),
                    extensions: vec!["csv".into()],
                    save: false,
                }),
        )
    }
    fn on_dialog_response(&mut self, response: DialogResponse) -> CommandStep {
        if let DialogResponse::Submitted(v) = response {
            if let Some(DialogValue::FilePath(p)) | Some(DialogValue::Text(p)) = v.get("src") {
                if !p.trim().is_empty() {
                    if let Ok(mut g) = PENDING_CSV.lock() {
                        *g = Some(PathBuf::from(p.trim()));
                    }
                    return CommandStep::Dispatch(format!("{} {}", self.cmd, PENDING_MARK));
                }
            }
        }
        CommandStep::Cancel
    }
}

/// Resolve the source CSV. `Some(path)` = go ahead; `None` = either the picker
/// was started (the command re-runs itself with the choice) or an error was shown.
fn resolve_csv(
    host: &mut dyn HostApi,
    arg: &str,
    cmd: &'static str,
    title: &'static str,
) -> Option<PathBuf> {
    let arg = arg.trim().trim_matches('"');
    if arg == PENDING_MARK {
        let p = PENDING_CSV.lock().ok().and_then(|mut g| g.take());
        if p.is_none() {
            host.push_error(&format!("{cmd}: no file was chosen"));
        }
        return p;
    }
    if !arg.is_empty() {
        return Some(PathBuf::from(arg));
    }
    let default_path = default_path(host)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    host.start_interactive(Box::new(PickCsv { cmd, title, default_path }));
    None
}

fn load_table(
    host: &mut dyn HostApi,
    arg: &str,
    cmd: &'static str,
    title: &'static str,
) -> Option<Table> {
    let path = resolve_csv(host, arg, cmd, title)?;
    host.push_output(&format!("{cmd}: reading {}", path.display()));
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            host.push_error(&format!("{cmd}: cannot read {}: {e}", path.display()));
            return None;
        }
    };
    match Table::parse(&text) {
        Ok(t) => {
            let col = |c: Option<usize>| c.map(|i| (i + 1).to_string()).unwrap_or("-".into());
            host.push_output(&format!(
                "{cmd}: {} data row(s); columns Handle={} Block={} Layer={} X={} Y={} Z={}; tags=[{}]",
                t.rows.len(),
                col(t.handle),
                col(t.block),
                col(t.layer),
                col(t.x),
                col(t.y),
                col(t.z),
                t.tags.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
            ));
            Some(t)
        }
        Err(e) => {
            host.push_error(&format!("{cmd}: {}: {e}", path.display()));
            None
        }
    }
}

const EPS: f64 = 1e-6;

/// Layers named in `wanted` that the drawing lacks (deduplicated, case-insensitive).
fn missing_layers(host: &dyn HostApi, wanted: impl Iterator<Item = String>) -> Vec<String> {
    let doc = host.document();
    let mut out: Vec<String> = Vec::new();
    for n in wanted {
        if !doc.layers.contains(&n) && !out.iter().any(|m| m.eq_ignore_ascii_case(&n)) {
            out.push(n);
        }
    }
    out
}

fn make_layers(host: &mut dyn HostApi, names: &[String]) {
    for n in names {
        let cfg = LayerConfig { name: n.clone(), ..Default::default() };
        match host.add_layer(cfg) {
            Some(_) => host.push_output(&format!("Layer created: {n}")),
            None => host.push_error(&format!("Could not create layer '{n}'")),
        }
    }
}

/// BA_UPDATE: write CSV values back onto existing INSERTs.
fn update_from_csv(host: &mut dyn HostApi, arg: &str) {
    let Some(t) = load_table(host, arg, "BA_UPDATE", "Update blocks from CSV") else {
        return;
    };
    if t.handle.is_none() && !(t.block.is_some() && t.x.is_some() && t.y.is_some()) {
        host.push_error("CSV needs a Handle column, or Block + X + Y columns, to match rows");
        return;
    }
    let selected: HashSet<_> = host.selection().into_iter().collect();
    let filtered = !selected.is_empty();

    // Phase 1: read-only pass, build the updated INSERT copies.
    let mut updates: Vec<EntityType> = Vec::new();
    let mut wanted_layers: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let (mut unmatched, mut changed_attrs, mut moved, mut relayered) = (0usize, 0usize, 0usize, 0usize);
    let mut used = HashSet::new();
    let candidates;
    {
        let doc = host.document();
        let inserts: Vec<_> = doc
            .entities()
            .filter_map(|e| match e {
                EntityType::Insert(i) if !filtered || selected.contains(&i.common.handle) => {
                    Some(i)
                }
                _ => None,
            })
            .collect();
        candidates = inserts.len();
        for (n, row) in t.rows.iter().enumerate() {
            let hcell = t.cell(row, t.handle);
            let found = if !hcell.is_empty() {
                inserts
                    .iter()
                    .find(|i| i.common.handle.to_string().eq_ignore_ascii_case(hcell))
            } else if let Some((x, y, z)) = t.xyz(row) {
                let b = t.cell(row, t.block);
                inserts.iter().find(|i| {
                    !used.contains(&i.common.handle)
                        && i.block_name.eq_ignore_ascii_case(b)
                        && (i.insert_point.x - x).abs() < EPS
                        && (i.insert_point.y - y).abs() < EPS
                        && (i.insert_point.z - z).abs() < EPS
                })
            } else {
                None
            };
            let Some(ins) = found else {
                unmatched += 1;
                if notes.len() < 10 {
                    notes.push(format!(
                        "row {}: no match (handle='{}', block='{}', xyz={:?})",
                        n + 2,
                        hcell,
                        t.cell(row, t.block),
                        t.xyz(row)
                    ));
                }
                continue;
            };
            used.insert(ins.common.handle);
            let mut new = (*ins).clone();
            let mut dirty = false;

            // Position: only when matched by Handle (otherwise it already matches).
            if !hcell.is_empty() {
                if let Some((x, y, z)) = t.xyz(row) {
                    let d = Vector3::new(
                        x - new.insert_point.x,
                        y - new.insert_point.y,
                        z - new.insert_point.z,
                    );
                    if d.x.abs() > EPS || d.y.abs() > EPS || d.z.abs() > EPS {
                        new.insert_point = Vector3::new(x, y, z);
                        // Attributes sit in WCS: carry them along with the block.
                        for a in &mut new.attributes {
                            a.insertion_point = a.insertion_point + d;
                            a.alignment_point = a.alignment_point + d;
                        }
                        moved += 1;
                        dirty = true;
                    }
                }
            }
            // Layer.
            let lname = t.cell(row, t.layer);
            if !lname.is_empty() && !new.common.layer.eq_ignore_ascii_case(lname) {
                new.common.layer = lname.to_string();
                wanted_layers.push(lname.to_string());
                relayered += 1;
                dirty = true;
            }
            // Attribute values.
            let mut attr_changed = false;
            for a in &mut new.attributes {
                if let Some(v) = t.tag_value(row, &a.tag) {
                    if a.value != v {
                        a.set_value(v);
                        attr_changed = true;
                    }
                }
            }
            if attr_changed {
                changed_attrs += 1;
                dirty = true;
            }
            if dirty {
                updates.push(EntityType::Insert(new));
            }
        }
    }
    host.push_output(&format!(
        "BA_UPDATE: {} block reference(s) in scope{}, {} row(s) to apply",
        candidates,
        if filtered { " (selection)" } else { "" },
        updates.len()
    ));
    for n in &notes {
        host.push_error(n);
    }

    // Phase 2: commit as one undo step.
    let count = updates.len();
    if count > 0 {
        host.begin_undo_group("BA_UPDATE");
        let new_layers = missing_layers(host, wanted_layers.into_iter());
        make_layers(host, &new_layers);
        let mut failed = 0;
        for e in updates {
            if !host.update_entity(e) {
                failed += 1;
            }
        }
        host.end_undo_group();
        host.bump_geometry();
        host.set_dirty();
        if failed > 0 {
            host.push_error(&format!("BA_UPDATE: host refused {failed} update(s)"));
        }
    }
    host.push_info(&format!(
        "BA_UPDATE: {} block(s) updated ({} attribute change(s), {} moved, {} re-layered), {} row(s) unmatched",
        count, changed_attrs, moved, relayered, unmatched
    ));
}

/// BA_CREATE: one new INSERT per CSV row, attributes built from the block's ATTDEFs.
fn create_from_csv(host: &mut dyn HostApi, arg: &str) {
    let Some(t) = load_table(host, arg, "BA_CREATE", "Create blocks from CSV") else {
        return;
    };
    if t.block.is_none() || t.x.is_none() || t.y.is_none() {
        host.push_error("CSV needs BLOCK, X and Y columns (Z optional)");
        return;
    }
    let mut new_ents: Vec<EntityType> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    let mut wanted_layers: Vec<String> = Vec::new();
    {
        let doc = host.document();
        for (n, row) in t.rows.iter().enumerate() {
            let line = n + 2;
            let bname = t.cell(row, t.block);
            let Some((x, y, z)) = t.xyz(row) else {
                errors.push(format!("line {line}: bad X/Y/Z"));
                continue;
            };
            let Some(br) = doc.block_records.get(bname) else {
                errors.push(format!("line {line}: block '{bname}' not defined in drawing"));
                continue;
            };
            let base = match doc.get_entity(br.block_entity_handle) {
                Some(EntityType::Block(b)) => b.base_point,
                _ => br.base_point,
            };
            let ip = Vector3::new(x, y, z);
            let mut ins = Insert::new(br.name.clone(), ip);
            let lname = t.cell(row, t.layer);
            if !lname.is_empty() {
                ins.common.layer = lname.to_string();
                wanted_layers.push(lname.to_string());
            }
            for h in &br.entity_handles {
                if let Some(EntityType::AttributeDefinition(ad)) = doc.get_entity(*h) {
                    if ad.flags.constant {
                        continue;
                    }
                    let val = t.tag_value(row, &ad.tag).map(|v| v.to_string());
                    let mut a = AttributeEntity::from_definition(ad, val);
                    // Block space -> WCS (no scale/rotation): shift by insert - base.
                    let d = ip - base;
                    a.insertion_point = a.insertion_point + d;
                    a.alignment_point = a.alignment_point + d;
                    ins.attributes.push(a);
                }
            }
            new_ents.push(EntityType::Insert(ins));
        }
    }
    let count = new_ents.len();
    if count > 0 {
        host.begin_undo_group("BA_CREATE");
        let new_layers = missing_layers(host, wanted_layers.into_iter());
        make_layers(host, &new_layers);
        let handles = host.add_entities(new_ents);
        host.end_undo_group();
        host.bump_geometry();
        host.set_dirty();
        if handles.len() != count {
            host.push_error(&format!(
                "BA_CREATE: host returned {} handle(s) for {count} block(s)",
                handles.len()
            ));
        }
    }
    for e in errors.iter().take(10) {
        host.push_error(e);
    }
    if errors.len() > 10 {
        host.push_error(&format!("... and {} more", errors.len() - 10));
    }
    host.push_info(&format!(
        "BA_CREATE: {count} block(s) created, {} row(s) skipped",
        errors.len()
    ));
}

/// BA_DEBUG: dump what the plugin can see, to diagnose "nothing happened".
fn debug_dump(host: &mut dyn HostApi) {
    let sel = host.selection().len();
    let path = host.document_path(host.tab_id());
    let mut lines: Vec<String> = Vec::new();
    {
        let doc = host.document();
        let n_all = doc.entities().count();
        let n_ins = doc
            .entities()
            .filter(|e| matches!(e, EntityType::Insert(_)))
            .count();
        lines.push(format!("plugin v{} | api {:?}", env!("CARGO_PKG_VERSION"), ApiVersion::CURRENT));
        lines.push(format!("entities={n_all} inserts={n_ins} selection={sel}"));
        let layers: Vec<&str> = doc.layers.iter().map(|l| l.name.as_str()).collect();
        lines.push(format!("layers({}): {}", layers.len(), layers.join(", ")));
        for br in doc.block_records.iter() {
            if br.name.starts_with('*') {
                continue;
            }
            let tags: Vec<String> = br
                .entity_handles
                .iter()
                .filter_map(|h| match doc.get_entity(*h) {
                    Some(EntityType::AttributeDefinition(a)) => Some(a.tag.clone()),
                    _ => None,
                })
                .collect();
            lines.push(format!("block '{}': attdefs=[{}]", br.name, tags.join(", ")));
        }
    }
    host.push_output(&format!("drawing path: {:?}", path));
    for l in lines {
        host.push_output(&l);
    }
}

fn default_path(host: &dyn HostApi) -> Option<PathBuf> {
    let p = host.document_path(host.tab_id())?;
    Some(p.with_extension("attributes.csv"))
}

struct BlockAttrPlugin;

impl BuiltinPlugin for BlockAttrPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn ribbon(&self) -> Box<dyn CadModule> {
        Box::new(BlockAttrModule)
    }

    fn dispatch(&self, host: &mut dyn HostApi, cmd: &str) -> bool {
        let (name, arg) = match cmd.split_once(' ') {
            Some((n, a)) => (n, a.trim()),
            None => (cmd, ""),
        };
        match name {
            "BA_LIST" => {
                let (rows, filtered) = collect(host);
                host.push_info(&format!(
                    "{} block reference(s){}",
                    rows.len(),
                    if filtered { " (selection)" } else { "" }
                ));
                for r in &rows {
                    let a: Vec<String> =
                        r.attrs.iter().map(|(t, v)| format!("{t}={v}")).collect();
                    host.push_output(&format!("{} [{}] {}", r.block, r.handle, a.join("; ")));
                }
                true
            }
            "BA_EXPORT" => {
                let (rows, filtered) = collect(host);
                let path = if arg.is_empty() {
                    default_path(host)
                } else {
                    Some(PathBuf::from(arg))
                };
                match path {
                    None => host.push_error("Drawing is unsaved: save it or run BA_EXPORT <path>"),
                    Some(p) => match std::fs::write(&p, to_csv(&rows)) {
                        Ok(()) => host.push_info(&format!(
                            "Wrote {} row(s){} to {}",
                            rows.len(),
                            if filtered { " (selection)" } else { "" },
                            p.display()
                        )),
                        Err(e) => host.push_error(&format!("Write failed: {e}")),
                    },
                }
                true
            }
            "BA_UPDATE" => {
                update_from_csv(host, arg);
                true
            }
            "BA_CREATE" => {
                create_from_csv(host, arg);
                true
            }
            "BA_DEBUG" => {
                debug_dump(host);
                true
            }
            _ => false,
        }
    }
}

struct BlockAttrModule;

impl CadModule for BlockAttrModule {
    fn id(&self) -> &'static str {
        MANIFEST.id
    }
    fn title(&self) -> &'static str {
        "Attributes"
    }
    fn ribbon_groups(&self) -> &[RibbonGroup] {
        static GROUPS: std::sync::OnceLock<Vec<RibbonGroup>> = std::sync::OnceLock::new();
        GROUPS.get_or_init(|| {
            vec![RibbonGroup {
                title: "Extract",
                tools: vec![
                    RibbonItem::LargeTool(ToolDef {
                        id: "BA_LIST",
                        label: "List",
                        icon: IconKind::Glyph("L"),
                        event: ModuleEvent::Command("BA_LIST".to_string()),
                    }),
                    RibbonItem::LargeTool(ToolDef {
                        id: "BA_EXPORT",
                        label: "To CSV",
                        icon: IconKind::Glyph("C"),
                        event: ModuleEvent::Command("BA_EXPORT".to_string()),
                    }),
                    RibbonItem::LargeTool(ToolDef {
                        id: "BA_UPDATE",
                        label: "Update From CSV",
                        icon: IconKind::Glyph("U"),
                        event: ModuleEvent::Command("BA_UPDATE".to_string()),
                    }),
                    RibbonItem::LargeTool(ToolDef {
                        id: "BA_CREATE",
                        label: "Create From CSV",
                        icon: IconKind::Glyph("+"),
                        event: ModuleEvent::Command("BA_CREATE".to_string()),
                    }),
                ],
            }]
        })
    }
}

export_plugin!(BlockAttrPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sample_and_roundtrip_export() {
        let t = Table::parse(
            "BLOCK,X,Y,Z,BH\nBH,231.426,215.776,10,BH3\nBH,199.4,249.6,11,\"B,H\"\"2\"\n\n",
        )
        .unwrap();
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.tags, vec![("BH".to_string(), 4)]);
        assert_eq!(t.xyz(&t.rows[0]), Some((231.426, 215.776, 10.0)));
        assert_eq!(t.tag_value(&t.rows[1], "bh"), Some("B,H\"2"));
        let e = Table::parse("Handle,Block,Layer,X,Y,Z,BH\r\nA1,BH,0,1,2,3,v\r\n").unwrap();
        assert_eq!((e.handle, e.x, e.tags.len()), (Some(0), Some(3), 1));
    }
}
