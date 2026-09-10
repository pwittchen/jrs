package com.example.cart;

/** A discount: how much it takes off a cart, in cents. */
@FunctionalInterface
public interface Discount {
    long cents(Cart cart, long subtotalCents);

    /** A percentage off everything. */
    static Discount percent(int percent) {
        if (percent < 0 || percent > 100) {
            throw new IllegalArgumentException("not a percentage: " + percent);
        }
        return (cart, subtotal) -> subtotal * percent / 100;
    }

    /** Every {@code n}th unit of a line free. */
    static Discount everyNthFree(int n) {
        return (cart, subtotal) ->
                cart.items().stream().mapToLong(i -> (i.quantity() / n) * i.unitCents()).sum();
    }

    /** A fixed amount off, once the subtotal reaches a threshold. */
    static Discount amountOver(long thresholdCents, long offCents) {
        return (cart, subtotal) -> subtotal >= thresholdCents ? Math.min(offCents, subtotal) : 0;
    }
}
