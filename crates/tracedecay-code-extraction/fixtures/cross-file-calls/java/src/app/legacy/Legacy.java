package app.legacy;

public final class Legacy {
    public static String normalize(String text) {
        return text.toLowerCase();
    }

    public static String oldFormat(String value) {
        return "<" + normalize(value) + ">";
    }
}
