package app;

import app.compat.Compat;
import app.legacy.Legacy;
import app.math.MathOps;
import app.report.Report;
import app.shapes.Shapes;
import app.store.Store;

public final class Main {
    public static void main(String[] args) {
        Store store = new Store();
        store.add("Key", 1);
        store.get("key");
        System.out.println(Report.summary(new int[] {1, 2, 3}));
        System.out.println(Compat.upgrade(" Text "));
        System.out.println(Shapes.area(3, 4) + " " + Shapes.perimeter(3, 4));
        System.out.println(Legacy.oldFormat("x") + " " + MathOps.scale(2, 3));
    }
}
