package com.example.ledger.core;

import java.time.LocalDate;
import java.util.List;
import java.util.Map;
import java.util.TreeMap;

/**
 * A dated set of postings that balances: in every currency, what goes into
 * accounts comes out of others.
 */
public record Transaction(LocalDate date, String description, List<Posting> postings) {

    public Transaction {
        postings = List.copyOf(postings);
        if (postings.size() < 2) {
            throw new IllegalArgumentException("a transaction moves money between at least two accounts");
        }
        Map<String, Money> sums = new TreeMap<>();
        for (Posting posting : postings) {
            Money amount = posting.amount();
            sums.merge(amount.currency(), amount, Money::plus);
        }
        for (Money sum : sums.values()) {
            if (!sum.isZero()) {
                throw new UnbalancedException(description, sum);
            }
        }
    }

    /** A transaction whose postings do not sum to zero. */
    public static final class UnbalancedException extends IllegalArgumentException {
        private static final long serialVersionUID = 1L;

        UnbalancedException(String description, Money off) {
            super("\"" + description + "\" does not balance: it is off by " + off);
        }
    }
}
