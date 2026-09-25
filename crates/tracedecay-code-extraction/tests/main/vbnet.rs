use tracedecay_code_extraction::LanguageExtractor;
use tracedecay_code_extraction::VbNetExtractor;
use tracedecay_domain::*;

#[test]
fn test_vb_class_docstring() {
    let source = r#"
''' <summary>
''' A test class.
''' </summary>
Class MyClass
End Class
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;
    let class = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name == "MyClass")
        .expect("MyClass not found");
    assert_eq!(class.docstring.as_deref(), Some("A test class."));
}

#[test]
fn test_vb_interface() {
    let source = r#"
Interface ISerializable
    Function ToJson() As String
End Interface
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;
    let interfaces: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Interface)
        .collect();
    assert_eq!(interfaces.len(), 1);
    assert_eq!(interfaces[0].name, "ISerializable");
}

#[test]
fn test_vb_struct() {
    let source = r#"
Structure Point
    Public X As Double
    Public Y As Double
End Structure
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;
    let structs: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Struct)
        .collect();
    assert_eq!(structs.len(), 1);
    assert_eq!(structs[0].name, "Point");
}

#[test]
fn test_vb_module() {
    let source = r#"
Module Helpers
    Sub LogMessage(msg As String)
        Console.WriteLine(msg)
    End Sub
End Module
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;
    let modules: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Module)
        .collect();
    assert_eq!(modules.len(), 1);
    assert_eq!(modules[0].name, "Helpers");
}

#[test]
fn test_vb_enum_with_variants() {
    let source = r#"
Enum LogLevel
    Debug
    Info
    Warning
End Enum
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;

    let enums: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Enum)
        .collect();
    assert_eq!(enums.len(), 1);
    assert_eq!(enums[0].name, "LogLevel");

    let variants: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::EnumVariant)
        .collect();
    assert_eq!(variants.len(), 3);
    assert!(variants.iter().any(|v| v.name == "Debug"));
    assert!(variants.iter().any(|v| v.name == "Info"));
    assert!(variants.iter().any(|v| v.name == "Warning"));
}

#[test]
fn test_vb_methods() {
    let source = r#"
Class Foo
    Public Function GetValue() As Integer
        Return 42
    End Function

    Public Sub DoWork()
        Console.WriteLine("working")
    End Sub
End Class
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;

    let methods: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method)
        .collect();
    assert_eq!(
        methods.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
        ["GetValue", "DoWork"]
    );
    assert!(methods.iter().any(|m| m.name == "GetValue"));
    assert!(methods.iter().any(|m| m.name == "DoWork"));
}

#[test]
fn test_vb_constructor() {
    let source = r#"
Class Foo
    Sub New(name As String)
        Console.WriteLine(name)
    End Sub
End Class
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;

    let ctors: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Constructor)
        .collect();
    assert_eq!(ctors.len(), 1);
    assert_eq!(ctors[0].name, "New");
}

#[test]
fn test_vb_properties() {
    let source = r#"
Class Foo
    Public Property Name As String
    Public ReadOnly Property Id As Integer
End Class
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;

    let props: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property)
        .collect();
    assert_eq!(
        props.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
        ["Name", "Id"]
    );
    assert!(props.iter().any(|p| p.name == "Name"));
    assert!(props.iter().any(|p| p.name == "Id"));
}

#[test]
fn test_vb_const() {
    let source = r#"
Const MaxConnections As Integer = 100
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;

    let consts: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Const)
        .collect();
    assert_eq!(consts.len(), 1);
    assert_eq!(consts[0].name, "MaxConnections");
}

#[test]
fn test_vb_method_visibility() {
    let source = r#"
Class Foo
    Public Sub PublicMethod()
    End Sub

    Private Sub PrivateMethod()
    End Sub
End Class
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;

    let pub_method = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Method && n.name == "PublicMethod");
    assert!(pub_method.is_some());
    assert_eq!(pub_method.unwrap().visibility, Visibility::Pub);

    let priv_method = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Method && n.name == "PrivateMethod");
    assert!(priv_method.is_some());
    assert_eq!(priv_method.unwrap().visibility, Visibility::Private);
}

#[test]
fn test_vb_fields() {
    let source = r#"
Class Foo
    Private _value As Integer
End Class
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("test.vb", source).result;

    let fields: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Field)
        .collect();
    assert!(
        fields.iter().any(|f| f.name == "_value"),
        "Expected _value field, got: {:?}",
        fields.iter().map(|f| &f.name).collect::<Vec<_>>()
    );
}

#[test]
fn test_vb_attributes_on_class_and_method() {
    let source = r#"
<Serializable>
<Obsolete("message")>
Class MyClass
    <TestMethod>
    Sub DoSomething()
    End Sub
End Class
"#;
    let extractor = VbNetExtractor;
    let result = extractor.extract_artifact("attr.vb", source).result;

    // Should have 3 AnnotationUsage nodes: Serializable, Obsolete, TestMethod
    let annots: Vec<_> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::AnnotationUsage)
        .collect();
    assert_eq!(
        annots.len(),
        3,
        "expected 3 annotations, got: {:?}",
        annots.iter().map(|a| &a.name).collect::<Vec<_>>()
    );
    assert!(annots.iter().any(|a| a.name == "Serializable"));
    assert!(annots.iter().any(|a| a.name == "Obsolete"));
    assert!(annots.iter().any(|a| a.name == "TestMethod"));

    // Should have Annotates edges.
    let annotates_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Annotates)
        .collect();
    assert_eq!(annotates_edges.len(), 3, "expected 3 Annotates edges");

    // Should have Annotates unresolved refs.
    let annot_refs: Vec<_> = result
        .unresolved_refs
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Annotates)
        .collect();
    assert_eq!(annot_refs.len(), 3, "expected 3 Annotates refs");
}
