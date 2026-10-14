package com.example.ledger.core;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import java.util.SortedMap;
import java.util.TreeMap;

/** Transactions in the order they were recorded, and the balances they add up to. */
public final class Ledger {

    private final List<Transaction> transactions = new ArrayList<>();

    public Ledger record(Transaction transaction) {
        transactions.add(transaction);
        return this;
    }

    public List<Transaction> transactions() {
        return Collections.unmodifiableList(transactions);
    }

    /**
     * Each account's balance, and each parent account's as the sum of the
     * accounts under it: {@code assets} holds {@code assets:bank} and
     * {@code assets:cash}. Keyed by account, then currency.
     */
    public SortedMap<String, SortedMap<String, Money>> balances() {
        SortedMap<String, SortedMap<String, Money>> balances = new TreeMap<>();
        for (Transaction transaction : transactions) {
            for (Posting posting : transaction.postings()) {
                String account = posting.account();
                while (true) {
                    balances.computeIfAbsent(account, a -> new TreeMap<>())
                            .merge(posting.amount().currency(), posting.amount(), Money::plus);
                    int colon = account.lastIndexOf(':');
                    if (colon < 0) {
                        break;
                    }
                    account = account.substring(0, colon);
                }
            }
        }
        return balances;
    }
}
