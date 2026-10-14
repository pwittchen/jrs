package com.example.ledger.core;

import java.math.BigDecimal;
import java.util.Objects;

/** An amount in one currency, kept in minor units so that sums are exact. */
public record Money(long cents, String currency) implements Comparable<Money> {

    public Money {
        Objects.requireNonNull(currency, "currency");
        if (!currency.matches("[A-Z]{3}")) {
            throw new IllegalArgumentException("not an ISO 4217 code: " + currency);
        }
    }

    /** Parse a decimal amount such as {@code -12.50}. */
    public static Money parse(String amount, String currency) {
        BigDecimal value = new BigDecimal(amount.trim());
        if (value.scale() > 2) {
            throw new IllegalArgumentException("more than two decimals: " + amount);
        }
        return new Money(value.movePointRight(2).longValueExact(), currency);
    }

    public static Money zero(String currency) {
        return new Money(0, currency);
    }

    public Money plus(Money other) {
        if (!currency.equals(other.currency)) {
            throw new IllegalArgumentException(currency + " and " + other.currency + " do not add up");
        }
        return new Money(Math.addExact(cents, other.cents), currency);
    }

    public Money negated() {
        return new Money(Math.negateExact(cents), currency);
    }

    public boolean isZero() {
        return cents == 0;
    }

    @Override
    public int compareTo(Money other) {
        int byCurrency = currency.compareTo(other.currency);
        return byCurrency != 0 ? byCurrency : Long.compare(cents, other.cents);
    }

    @Override
    public String toString() {
        return BigDecimal.valueOf(cents, 2).toPlainString() + " " + currency;
    }
}
