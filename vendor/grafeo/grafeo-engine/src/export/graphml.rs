//! GraphML graph export.
//!
//! Serializes LPG nodes and edges to the [GraphML](http://graphml.graphdrawing.org/) XML format,
//! readable by Gephi, Cytoscape, NetworkX, yEd, igraph, and other graph tools.

use std::io::Write;

use grafeo_core::graph::lpg::{Edge, Node};

use super::{
    ExportError, discover_edge_schema, discover_node_schema, escape_xml, value_to_graphml_type,
    value_to_xml_string,
};

/// Writes a complete GraphML document to the given writer.
///
/// Node labels are stored as a `_labels` data key, edge types as `_type`.
/// Node IDs are prefixed with `n`, edge IDs with `e` (GraphML convention).
///
/// # Errors
///
/// Returns [`ExportError::Io`] if writing fails.
pub fn write_graphml<W: Write>(
    writer: &mut W,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<(), ExportError> {
    let node_schema = discover_node_schema(nodes, value_to_graphml_type);
    let edge_schema = discover_edge_schema(edges, value_to_graphml_type);

    // XML header
    writeln!(writer, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    writeln!(
        writer,
        "<graphml xmlns=\"http://graphml.graphdrawing.org/xmlns\">"
    )?;

    // Key declarations for node properties
    // Reserve d0 for _labels (always present)
    writeln!(
        writer,
        "  <key id=\"d0\" for=\"node\" attr.name=\"_labels\" attr.type=\"string\"/>"
    )?;
    for (key, (id, type_str)) in &node_schema {
        // Offset by 1 to account for _labels at d0
        let key_id = id + 1;
        writeln!(
            writer,
            "  <key id=\"d{key_id}\" for=\"node\" attr.name=\"{}\" attr.type=\"{type_str}\"/>",
            escape_xml(key.as_str())
        )?;
    }

    // Key declarations for edge properties
    // Edge keys start after all node keys. Reserve one for _type.
    let edge_key_offset = node_schema.len() + 1;
    writeln!(
        writer,
        "  <key id=\"d{edge_key_offset}\" for=\"edge\" attr.name=\"_type\" attr.type=\"string\"/>"
    )?;
    for (key, (id, type_str)) in &edge_schema {
        let key_id = edge_key_offset + 1 + id;
        writeln!(
            writer,
            "  <key id=\"d{key_id}\" for=\"edge\" attr.name=\"{}\" attr.type=\"{type_str}\"/>",
            escape_xml(key.as_str())
        )?;
    }

    // Graph element
    writeln!(writer, "  <graph edgedefault=\"directed\">")?;

    // Nodes
    for node in nodes {
        writeln!(writer, "    <node id=\"n{}\">", node.id.0)?;

        // _labels data
        let labels: String = node
            .labels
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(",");
        writeln!(
            writer,
            "      <data key=\"d0\">{}</data>",
            escape_xml(&labels)
        )?;

        // Property data
        for (key, (schema_id, _)) in &node_schema {
            if let Some(value) = node.properties.get(key)
                && let Some(val_str) = value_to_xml_string(value)
            {
                let key_id = schema_id + 1;
                writeln!(writer, "      <data key=\"d{key_id}\">{val_str}</data>")?;
            }
        }

        writeln!(writer, "    </node>")?;
    }

    // Edges
    for edge in edges {
        writeln!(
            writer,
            "    <edge id=\"e{}\" source=\"n{}\" target=\"n{}\">",
            edge.id.0, edge.src.0, edge.dst.0
        )?;

        // _type data
        writeln!(
            writer,
            "      <data key=\"d{edge_key_offset}\">{}</data>",
            escape_xml(edge.edge_type.as_str())
        )?;

        // Property data
        for (key, (schema_id, _)) in &edge_schema {
            if let Some(value) = edge.properties.get(key)
                && let Some(val_str) = value_to_xml_string(value)
            {
                let key_id = edge_key_offset + 1 + schema_id;
                writeln!(writer, "      <data key=\"d{key_id}\">{val_str}</data>")?;
            }
        }

        writeln!(writer, "    </edge>")?;
    }

    // Close
    writeln!(writer, "  </graph>")?;
    writeln!(writer, "</graphml>")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeo_common::PropertyKey;
    use grafeo_common::types::PropertyMap;
    use grafeo_common::types::{EdgeId, NodeId, Value};

    fn make_node(id: u64, labels: &[&str], props: &[(&str, Value)]) -> Node {
        let mut properties = PropertyMap::new();
        for (k, v) in props {
            properties.insert(PropertyKey::from(*k), v.clone());
        }
        Node {
            id: NodeId(id),
            labels: labels.iter().map(|s| arcstr::ArcStr::from(*s)).collect(),
            properties,
        }
    }

    fn make_edge(id: u64, src: u64, dst: u64, edge_type: &str, props: &[(&str, Value)]) -> Edge {
        let mut properties = PropertyMap::new();
        for (k, v) in props {
            properties.insert(PropertyKey::from(*k), v.clone());
        }
        Edge {
            id: EdgeId(id),
            src: NodeId(src),
            dst: NodeId(dst),
            edge_type: arcstr::ArcStr::from(edge_type),
            properties,
        }
    }

    #[test]
    fn test_empty_graph() {
        let mut buf = Vec::new();
        write_graphml(&mut buf, &[], &[]).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("<graphml"));
        assert!(output.contains("<graph"));
        assert!(output.contains("</graph>"));
        assert!(output.contains("</graphml>"));
    }

    #[test]
    fn test_single_node_with_labels() {
        let nodes = vec![make_node(
            1,
            &["Person"],
            &[("name", Value::String("Alix".into()))],
        )];
        let mut buf = Vec::new();
        write_graphml(&mut buf, &nodes, &[]).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("id=\"n1\""));
        assert!(output.contains("<data key=\"d0\">Person</data>"));
        assert!(output.contains("attr.name=\"name\""));
        assert!(output.contains("<data key=\"d1\">Alix</data>"));
    }

    #[test]
    fn test_edge_with_type_and_properties() {
        let nodes = vec![
            make_node(1, &["Person"], &[]),
            make_node(2, &["Person"], &[]),
        ];
        let edges = vec![make_edge(
            0,
            1,
            2,
            "KNOWS",
            &[("since", Value::Int64(2020))],
        )];
        let mut buf = Vec::new();
        write_graphml(&mut buf, &nodes, &edges).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("source=\"n1\""));
        assert!(output.contains("target=\"n2\""));
        assert!(output.contains(">KNOWS</data>"));
        assert!(output.contains(">2020</data>"));
    }

    #[test]
    fn test_xml_escaping() {
        let nodes = vec![make_node(
            1,
            &["A&B"],
            &[("note", Value::String("<important>".into()))],
        )];
        let mut buf = Vec::new();
        write_graphml(&mut buf, &nodes, &[]).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains(">A&amp;B</data>"));
        assert!(output.contains(">&lt;important&gt;</data>"));
    }

    #[test]
    fn test_null_properties_omitted() {
        let nodes = vec![make_node(
            1,
            &["Person"],
            &[("name", Value::String("Gus".into())), ("age", Value::Null)],
        )];
        let mut buf = Vec::new();
        write_graphml(&mut buf, &nodes, &[]).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains(">Gus</data>"));
        // age key should be declared but no data element for the null value
        let data_count = output.matches("<data key=").count();
        // d0 = _labels, d1 = name (age is null, omitted)
        assert_eq!(data_count, 2);
    }

    #[test]
    fn test_key_id_namespacing() {
        // Verify node keys and edge keys don't collide
        let nodes = vec![make_node(
            1,
            &["Person"],
            &[("name", Value::String("Alix".into()))],
        )];
        let edges = vec![make_edge(
            0,
            1,
            1,
            "SELF",
            &[("weight", Value::Float64(1.0))],
        )];
        let mut buf = Vec::new();
        write_graphml(&mut buf, &nodes, &edges).unwrap();
        let output = String::from_utf8(buf).unwrap();

        // d0 = _labels (node), d1 = name (node), d2 = _type (edge), d3 = weight (edge)
        assert!(output.contains("id=\"d0\" for=\"node\" attr.name=\"_labels\""));
        assert!(output.contains("id=\"d1\" for=\"node\" attr.name=\"name\""));
        assert!(output.contains("id=\"d2\" for=\"edge\" attr.name=\"_type\""));
        assert!(output.contains("id=\"d3\" for=\"edge\" attr.name=\"weight\""));
    }
}
