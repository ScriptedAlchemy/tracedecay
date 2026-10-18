package app.compat;

import app.legacy.Legacy;
import app.util.Util;

public final class Compat {
    public static String shim(String text) {
        return Legacy.normalize(text);
    }

    public static String upgrade(String text) {
        return Util.normalize(shim(text));
    }
}
