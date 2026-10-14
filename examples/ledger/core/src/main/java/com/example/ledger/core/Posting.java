package com.example.ledger.core;

import java.util.Objects;

/** One leg of a transaction: an amount moved into (or, negative, out of) an account. */
public record Posting(String account, Money amount) {

    public Posting {
        Objects.requireNonNull(amount, "amount");
        if (account == null || !account.matches("[a-z][a-z0-9-]*(:[a-z][a-z0-9-]*)*")) {
            throw new IllegalArgumentException("not an account name like assets:bank: " + account);
        }
    }
}
