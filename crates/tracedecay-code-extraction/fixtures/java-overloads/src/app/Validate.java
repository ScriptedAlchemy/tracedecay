package app;

public final class Validate {
    public static void notNull(Object obj) {
        notNull(obj, "must not be null");
    }

    public static void notNull(Object obj, String msg) {
        if (obj == null) {
            throw new IllegalArgumentException(msg);
        }
    }

    public static void check(int value) {}

    public static void check(String value) {}

    public static String format(String pattern, Object... args) {
        return pattern;
    }

    public static String format(String pattern, String arg) {
        return pattern + arg;
    }

    static void selfCheck() {
        check(1);
    }
}
