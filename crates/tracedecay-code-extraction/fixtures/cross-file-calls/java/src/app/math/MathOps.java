package app.math;

import app.util.Util;

public final class MathOps {
    public static int total(int[] values) {
        int sum = 0;
        for (int value : values) {
            sum += value;
        }
        return sum;
    }

    public static int scale(int value, int factor) {
        return Util.clamp(value * factor, 0, 100);
    }
}
