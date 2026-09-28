package app.util;

public final class Util {
    public static String normalize(String text) {
        return text.strip().toLowerCase();
    }

    public static int clamp(int value, int low, int high) {
        return Math.max(low, Math.min(value, high));
    }
}
