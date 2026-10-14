package com.example.ledger.app;

import com.example.ledger.core.Ledger;
import com.example.ledger.core.Money;
import java.util.Map;
import java.util.SortedMap;

/** The balance report: every account, indented under its parent. */
final class Report {

    private Report() {}

    static String render(Ledger ledger) {
        StringBuilder out = new StringBuilder();
        for (Map.Entry<String, SortedMap<String, Money>> account : ledger.balances().entrySet()) {
            String name = account.getKey();
            int depth = name.length() - name.replace(":", "").length();
            String label = "  ".repeat(depth) + name.substring(name.lastIndexOf(':') + 1);
            for (Money balance : account.getValue().values()) {
                out.append(String.format("%-24s %14s%n", label, balance));
            }
        }
        return out.toString();
    }
}
