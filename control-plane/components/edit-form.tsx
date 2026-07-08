"use client";

import { startTransition, type ComponentProps } from "react";

// React 19 automatically resets a `<form action={fn}>` to its DOM defaults once
// the action settles. Edit forms seed their fields from server props
// (defaultValue / defaultChecked), so that reset snaps every field back to the
// pre-save values until the next full page load — the saved state only LOOKS
// lost. Submitting via onSubmit and dispatching the action inside a manual
// transition keeps React's reset out of the loop: the form shows exactly what
// was submitted while the revalidated data streams in. Use this for forms that
// EDIT an existing entity; create-style forms should keep `<form action>` so
// they clear after a successful add.
export function EditForm({
  action,
  ...props
}: Omit<ComponentProps<"form">, "action" | "onSubmit"> & {
  action: (formData: FormData) => void;
}) {
  return (
    <form
      {...props}
      onSubmit={(event) => {
        event.preventDefault();
        const formData = new FormData(event.currentTarget);
        startTransition(() => action(formData));
      }}
    />
  );
}
