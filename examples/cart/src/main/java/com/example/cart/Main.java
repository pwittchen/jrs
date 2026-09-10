package com.example.cart;

public final class Main {
    private Main() {}

    public static void main(String[] args) {
        Cart cart = new Cart()
                .add("coffee", 1200, 2)
                .add("croissant", 180, 6)
                .add("jam", 450, 1)
                .offer(Discount.percent(10))
                .offer(Discount.everyNthFree(3))
                .offer(Discount.amountOver(5000, 700));
        for (Cart.Item item : cart.items()) {
            System.out.printf("%-10s %2d x %6s%n", item.name(), item.quantity(), money(item.unitCents()));
        }
        System.out.printf("subtotal %15s%n", money(cart.subtotalCents()));
        System.out.printf("discount %15s%n", "-" + money(cart.discountCents()));
        System.out.printf("total    %15s%n", money(cart.totalCents()));
    }

    static String money(long cents) {
        return String.format("%d.%02d", cents / 100, cents % 100);
    }
}
