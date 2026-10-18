package app.gaps;

import java.util.List;

import app.util.Util;

public final class Gaps {
    public static List<String> probe(String text) {
        Util.absent(text);
        return List.of(text);
    }
}
