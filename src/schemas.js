import { z } from 'zod';

const personaSchema = z.enum(['hosteller', 'dayscholar']);
const gatePassSchema = z.object({
  status: z.enum(['none', 'pending', 'approved']),
  type: z.string().nullable(),
  early: z.boolean(),
  step: z.number().int().min(0),
});

export const appStateSchema = z.object({
  persona: personaSchema,
  gp: gatePassSchema,
  paid: z.object({
    tuition: z.boolean(),
    hostel: z.boolean(),
    transport: z.boolean(),
    exam: z.boolean(),
  }),
  pay: z.object({
    comp: z.string().nullable(),
    step: z.number().int().min(0),
    plan: z.string().nullable(),
    mode: z.string().nullable(),
  }),
  refunds: z.record(z.string(), z.string()),
  condonation: z.enum(['none', 'pending', 'approved']),
  examReg: z.number().int().min(0),
  reval: z.record(z.string(), z.string()),
  asg: z.object({ a3: z.string() }),
  changeNotice: z.boolean(),
  mess: z.boolean(),
  hostelLeave: z.number().int().min(0),
  hostelTickets: z.array(z.object({
    id: z.string(),
    cat: z.string(),
    text: z.string(),
    status: z.string(),
  })),
  tripStep: z.number().int().min(0),
  breakdown: z.boolean(),
  docReq: z.array(z.object({
    id: z.string(),
    type: z.string(),
    on: z.string(),
    status: z.string(),
  })),
  placeApp: z.number().int().min(0),
  feedback: z.number().int().min(0),
});

export const stateUpdateSchema = z.object({
  state: appStateSchema,
  action: z.string().trim().min(1).max(80).optional(),
});

export const loginSchema = z.object({
  email: z.string().trim().email().max(254).transform((value) => value.toLowerCase()),
  password: z.string().min(8).max(128),
});
