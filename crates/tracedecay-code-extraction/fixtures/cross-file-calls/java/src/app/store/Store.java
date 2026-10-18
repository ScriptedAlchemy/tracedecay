package app.store;

import java.util.HashMap;
import java.util.Map;

import app.util.Util;

public final class Store {
    private final Map<String, Integer> items = new HashMap<>();

    public void add(String key, int value) {
        items.put(Util.normalize(key), value);
    }

    public Integer get(String key) {
        return items.get(Util.normalize(key));
    }
}
