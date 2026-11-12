//! Block attribute extraction for Open CAD Studio.
//!
//! BA_LIST   - print every INSERT's attributes to the command line.
//! BA_EXPORT - write a CSV (one row per INSERT, one column per tag).
//!             `BA_EXPORT <path>` writes there; bare `BA_EXPORT` writes
//!             <drawing>.attributes.csv next to the saved drawing.

use ocs_plugin_api::export_plugin;
use ocs_plugin_api::host::{BuiltinPlugin, EntityType, HostApi};
use ocs_plugin_api::manifest::{ApiVersion, PluginManifest};
use ocs_plugin_api::ribbon::{CadModule, IconKind, ModuleEvent, RibbonGroup, RibbonItem, ToolDef};
use std::path::PathBuf;

static MANIFEST: PluginManifest = PluginManifest {
    id: "opencad.block_attributes",
    name: "Block Attributes",
    version: env!("CARGO_PKG_VERSION"),
    description: "Extract block attribute data to CSV.",
    api_version: ApiVersion::CURRENT,
    ribbon_order: 60,
    xdata_apps: &[],
    command_prefixes: &["BA_EXPORT", "BA_LIST"],
};

struct Row {
    handle: String,
    block: String,
    layer: String,
    attrs: Vec<(String, String)>, // (tag, value) in file order
}

/// Read every INSERT and its attributes from the cached document copy.
fn collect(host: &dyn HostApi) -> Vec<Row> {
    host.document()
        .entities()
        .filter_map(|e| match e {
            EntityType::Insert(ins) => Some(Row {
                handle: ins.common.handle.to_string(), // ASSUMED field path
                block: ins.block_name.clone(),
                layer: ins.common.layer.clone(),
                attrs: ins
                    .attributes
                    .iter()
                    .map(|a| (a.tag.clone(), a.value.clone())) // ASSUMED: `value`
                    .collect(),
            }),
            _ => None,
        })
        .collect()
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
    let mut out = String::from("Handle,Block,Layer");
    for t in &tags {
        out.push(',');
        out.push_str(&csv_field(t));
    }
    out.push('\n');
    for r in rows {
        out.push_str(&format!(
            "{},{},{}",
            csv_field(&r.handle),
            csv_field(&r.block),
            csv_field(&r.layer)
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
                let rows = collect(host);
                host.push_info(&format!("{} block reference(s)", rows.len()));
                for r in &rows {
                    let a: Vec<String> =
                        r.attrs.iter().map(|(t, v)| format!("{t}={v}")).collect();
                    host.push_output(&format!("{} [{}] {}", r.block, r.handle, a.join("; ")));
                }
                true
            }
            "BA_EXPORT" => {
                let rows = collect(host);
                let path = if arg.is_empty() {
                    default_path(host)
                } else {
                    Some(PathBuf::from(arg))
                };
                match path {
                    None => host.push_error("Drawing is unsaved: save it or run BA_EXPORT <path>"),
                    Some(p) => match std::fs::write(&p, to_csv(&rows)) {
                        Ok(()) => host.push_info(&format!(
                            "Wrote {} row(s) to {}",
                            rows.len(),
                            p.display()
                        )),
                        Err(e) => host.push_error(&format!("Write failed: {e}")),
                    },
                }
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
                ],
            }]
        })
    }
}

export_plugin!(BlockAttrPlugin);
