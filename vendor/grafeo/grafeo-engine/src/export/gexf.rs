//! GEXF 1.3 graph export.
//!
//! Serializes LPG nodes and edges to the [GEXF 1.3](https://gexf.net/1.3/) XML format,
//! readable by Gephi, Gephi Lite, NetworkX, and other graph tools.

use std::io::Write;

use grafeo_core::graph::lpg::{Edge, Node};

use super::{
    ExportError, discover_edge_schema, discover_node_schema, escape_xml, value_to_gexf_type,
    value_to_xml_string,
};

/// Writes a complete GEXF 1.3 document to the given writer.
///
/// # Errors
///
/// Returns [`ExportError::Io`] if writing fails.
pub fn write_gexf<W: Write>(
    writer: &mut W,
    nodes: &[Node],
    edges: &[Edge],
) -> Result<(), ExportError> {
    let node_schema = discover_node_schema(nodes, value_to_gexf_type);
    let edge_schema = discover_edge_schema(edges, value_to_gexf_type);

    // XML header
    writeln!(writer, "<?xml version=\"1.0\" encoding=\"UTF-8\"?>")?;
    writeln!(
        writer,
        "<gexf xmlns=\"http://gexf.net/1.3\" version=\"1.3\">"
    )?;
    writeln!(writer, "  <meta>")?;
    writeln!(writer, "    <creator>GrafeoDB</creator>")?;
    writeln!(writer, "  </meta>")?;
    writeln!(
        writer,
        "  <graph defaultedgetype=\"directed\" mode=\"static\">"
    )?;

    // Node attribute declarations
    if !node_schema.is_empty() {
        writeln!(writer, "    <attributes class=\"node\">")?;
        for (key, (id, type_str)) in &node_schema {
            writeln!(
                writer,
                "      <attribute id=\"{id}\" title=\"{}\" type=\"{type_str}\"/>",
                escape_xml(key.as_str())
            )?;
        }
        writeln!(writer, "    </attributes>")?;
    }

    // Edge attribute declarations
    if !edge_schema.is_empty() {
        writeln!(writer, "    <attributes class=\"edge\">")?;
        for (key, (id, type_str)) in &edge_schema {
            writeln!(
                writer,
                "      <attribute id=\"{id}\" title=\"{}\" type=\"{type_str}\"/>",
                escape_xml(key.as_str())
            )?;
        }
        writeln!(writer, "    </attributes>")?;
    }

    // Nodes
    writeln!(writer, "    <nodes>")?;
    for node in nodes {
        let label: String = node
            .labels
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(",");
        write!(
            writer,
            "      <node id=\"{}\" label=\"{}\"",
            node.id.0,
            escape_xml(&label)
        )?;

        // Collect non-null attribute values
        let attvalues: Vec<_> = node_schema
            .iter()
            .filter_map(|(key, (id, _))| {
                node.properties
                    .get(key)
                    .and_then(|v| value_to_xml_string(v).map(|s| (*id, s)))
            })
            .collect();

        if attvalues.is_empty() {
            writeln!(writer, "/>")?;
        } else {
            writeln!(writer, ">")?;
            writeln!(writer, "        <attvalues>")?;
            for (id, val_str) in &attvalues {
                writeln!(
                    writer,
                    "          <attvalue for=\"{id}\" value=\"{}\"/>",
                    val_str
                )?;
            }
            writeln!(writer, "        </attvalues>")?;
            writeln!(writer, "      </node>")?;
        }
    }
    writeln!(writer, "    </nodes>")?;

    // Edges
    writeln!(writer, "    <edges>")?;
    for edge in edges {
        write!(
            writer,
            "      <edge id=\"{}\" source=\"{}\" target=\"{}\" label=\"{}\"",
            edge.id.0,
            edge.src.0,
            edge.dst.0,
            escape_xml(edge.edge_type.as_str())
        )?;

        let attvalues: Vec<_> = edge_schema
            .iter()
            .filter_map(|(key, (id, _))| {
                edge.properties
                    .get(key)
                    .and_then(|v| value_to_xml_string(v).map(|s| (*id, s)))
            })
            .collect();

        if attvalues.is_empty() {
            writeln!(writer, "/>")?;
        } else {
            writeln!(writer, ">")?;
            writeln!(writer, "        <attvalues>")?;
            for (id, val_str) in &attvalues {
                writeln!(
                    writer,
                    "          <attvalue for=\"{id}\" value=\"{}\"/>",
                    val_str
                )?;
            }
            writeln!(writer, "        </attvalues>")?;
            writeln!(writer, "      </edge>")?;
        }
    }
    writeln!(writer, "    </edges>")?;

    // Close
    writeln!(writer, "  </graph>")?;
    writeln!(writer, "</gexf>")?;

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
        write_gexf(&mut buf, &[], &[]).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("<gexf"));
        assert!(output.contains("<nodes>"));
        assert!(output.contains("</nodes>"));
        assert!(output.contains("<edges>"));
        assert!(output.contains("</edges>"));
    }

    #[test]
    fn test_single_node() {
        let nodes = vec![make_node(
            1,
            &["Person"],
            &[("name", Value::String("Alix".into()))],
        )];
        let mut buf = Vec::new();
        write_gexf(&mut buf, &nodes, &[]).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("id=\"1\""));
        assert!(output.contains("label=\"Person\""));
        assert!(output.contains("title=\"name\""));
        assert!(output.contains("value=\"Alix\""));
    }

    #[test]
    fn test_node_with_multiple_labels() {
        let nodes = vec![make_node(1, &["Person", "Employee"], &[])];
        let mut buf = Vec::new();
        write_gexf(&mut buf, &nodes, &[]).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("label=\"Person,Employee\""));
    }

    #[test]
    fn test_edge_with_properties() {
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
        write_gexf(&mut buf, &nodes, &edges).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("source=\"1\""));
        assert!(output.contains("target=\"2\""));
        assert!(output.contains("label=\"KNOWS\""));
        assert!(output.contains("value=\"2020\""));
    }

    #[test]
    fn test_xml_escaping_in_values() {
        let nodes = vec![make_node(
            1,
            &["Type<A>"],
            &[("desc", Value::String("a & b".into()))],
        )];
        let mut buf = Vec::new();
        write_gexf(&mut buf, &nodes, &[]).unwrap();
        let output = String::from_utf8(buf).unwrap();
        assert!(output.contains("label=\"Type&lt;A&gt;\""));
        assert!(output.contains("value=\"a &amp; b\""));
    }

    #[test]
    fn test_null_properties_omitted() {
        let nodes = vec![make_node(
            1,
            &["Person"],
            &[("name", Value::String("Alix".into())), ("age", Value::Null)],
        )];
        let mut buf = Vec::new();
        write_gexf(&mut buf, &nodes, &[]).unwrap();
        let output = String::from_utf8(buf).unwrap();
        // name should be present, age attvalue should be omitted
        assert!(output.contains("value=\"Alix\""));
        // There should be only one attvalue entry (name), not two
        assert_eq!(output.matches("attvalue for=").count(), 1);
    }
}
