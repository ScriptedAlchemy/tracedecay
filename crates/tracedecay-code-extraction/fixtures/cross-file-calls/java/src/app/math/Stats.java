package app.math;

public final class Stats {
    public static int mean(int[] values) {
        return MathOps.total(values) / values.length;
    }
}
