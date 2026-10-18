package app.report;

import app.math.Stats;
import app.util.Util;

public final class Report {
    public static String formatLine(String value) {
        return Util.normalize(value) + "\n";
    }

    public static String summary(int[] values) {
        return formatLine(String.valueOf(Stats.mean(values)));
    }
}
